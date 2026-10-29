# SPEC-007: Планировщик MIDI-событий (тик → сэмпл, опрос в узле)

Статус: ready
Владелец: команда движка
Для агента: разрешено реализовывать

Файлы: `crates/audio-core/src/schedule.rs` (новый модуль),
`crates/audio-core/src/{lib.rs,graph.rs}`, `crates/daw-app/src/nodes.rs`,
`crates/audio-core/tests/`, `docs/MIDI.md`, `docs/REALTIME.md`

## Проблема

`audio_core::midi` умеет разобрать SMF в отсортированный список событий в
тиках, но в аудиопотоке его применить нечем. Сейчас единственный способ
«сыграть» последовательность — заранее превратить её в буфер в контрольном
потоке. Это исключает транспорт: нет `Play`/`Stop`/`Seek`, нет смены темпа
на лету, и события нельзя опрашивать внутри узла (например, полифоническому
синтезатору, у которого голос стартует ровно на нужном сэмпле).

Нужен посредник: **события переводятся в сэмплы один раз в контрольном
потоке**, а аудиопоток в каждом кванте забирает те, что попали в квант.

## Поведение

1. Новый модуль `audio-core/src/schedule.rs`, публичный как
   `audio_core::schedule` и реэкспортированный из корня: `ScheduledEvent`,
   `Schedule`, `ScheduleSpec`.

   ```rust
   pub struct ScheduledEvent {
       pub sample: u64,          // абсолютная позиция от начала последовательности
       pub source: MidiEvent,    // исходное событие как пришло из парсера
   }

   pub struct ScheduleSpec {
       pub events: Vec<ScheduledEvent>,   // отсортировано по (sample, track, order_in_track)
       pub length_samples: u64,           // длина последовательности в сэмплах
   }
   ```

   `ScheduledEvent` и `ScheduleSpec` — plain data, без инвариантов на лету.

2. **Тик → сэмпл.** Только в контрольном потоке:

   ```rust
   impl ScheduleSpec {
       pub fn from_midi(
           file: &MidiFile,
           sample_rate: u32,
           start_tempo: Tempo,
           tail_samples: u64,
       ) -> Result<ScheduleSpec, ScheduleError>;
   }
   ```

   - «голова» расписания — `start_tempo` (текущий темп транспорта); темп
     меняется *только* событием `MidiMessage::Tempo` и действует на все
     последующие события;
   - формула (секунды копятся в `f64`, сэмпл округляется один раз в конце):

     ```text
     bpm(tick)     = 60_000_000 / micros_per_quarter   (или start_tempo)
     seconds(tick) = seconds(prev) + (tick - prev) / (ppqn * bpm(tick) / 60)
     sample        = round(seconds(tick) * sample_rate)
     ```

     Копим именно секунды, а не сэмплы: округление не накапливается, и смена
     темпа на середине не сдвигает уже посчитанное;
   - `Tempo { micros_per_quarter }` переводится в `bpm` через
     `60_000_000 / micros_per_quarter`; `micros_per_quarter == 0` →
     `ScheduleError::InvalidTempo`;
   - `ppqn == 0` → `ScheduleError::InvalidPpqn`;
   - `sample_rate == 0` → `ScheduleError::InvalidSampleRate`;
   - `Tempo`-события попадают в `events` как есть (тип `MidiMessage`
     сохраняется), их `sample` считается по темпу **до** них: смена темпа
     действует с этого тика вперёд;
   - два события на одном тике: `order_in_track` определяет порядок, как в
     парсере.

   `ScheduleError`: `InvalidPpqn`, `InvalidTempo { tick: u64 }`,
   `InvalidSampleRate`, `Empty`.

3. `length_samples` — сэмпл последнего события плюс `tail_samples` (хвост
   отпускания). `from_midi` возвращает `ScheduleError::Empty`, если в файле
   нет ни одного `NoteOn` (одних `Tempo` недостаточно).

4. **Исполнение в аудиопотоке.** `Schedule` — копия `ScheduleSpec`,
   созданная один раз в контрольном потоке:

   ```rust
   impl Schedule {
       pub fn new(spec: ScheduleSpec) -> Self;             // контрольный поток
       pub fn len(&self) -> usize;
       pub fn is_empty(&self) -> bool;
       pub fn events(&self) -> &[ScheduledEvent];          // только для тестов/UI
   }
   ```

   `Schedule` не аллоцирует после `new`; `Vec` переезжает по владению
   (`ScheduleSpec.events` move), а не копируется.

5. **Опрос.** У узла нет доступа к `Vec`: он получает `ScheduleCursor`,
   который двигается только вперёд и не может «прыгнуть назад».

   ```rust
   #[derive(Debug, Clone, Copy, Default)]
   pub struct ScheduleCursor { next: usize }

   impl Schedule {
       pub fn cursor(&self) -> ScheduleCursor;
       pub fn next_event(
           &self,
           cursor: &mut ScheduleCursor,
           position: u64,
           frames: usize,
       ) -> Option<ScheduledEvent>;
   }
   ```

   `cursor()` возвращает курсор в начале: позиция не вычисляется, «пустого»
   курсора нет (`Default` == начало).

   `next_event` возвращает следующее событие с `sample` в полуоткрытом
   диапазоне `[position, position + frames)` и продвигает курсор через него.
   События с `sample < position` (уже проиграны) пропускаются и тоже
   двигают курсор; повторный вызов в том же кванте вернёт следующее событие
   диапазона или `None`. Переполнение `position + frames` — через
   `saturating_add`.

   `next_event` не аллоцирует, не блокирует, не паникует: цикл вида
   `while cursor.next < events.len()`, чтение по индексу, без итераторов с
   замыканиями и без клонирования `Vec`.

6. **`Seek`.** Курсор не знает про транспорт: позиция приходит в
   `next_event` каждый квант.
   - `Seek` **вперёд**: события между старой и новой позицией пропускаются
     (курсор их проедет и они не проиграются) — это и есть «догоняющее»
     поведение без гонки.
   - `Seek` **назад**: курсор уже за старыми событиями и вернуть их не
     может, поэтому узел сбрасывает курсор: `cursor = schedule.cursor()`.
     Ответственность лежит на узле (он единственный видит `ctx.position`),
     граф в это не вмешивается.

7. **Пример узла.** `daw_app::nodes::MidiPlayer`:

   ```rust
   pub struct MidiPlayer {
       schedule: Schedule,
       cursor: ScheduleCursor,
       last_position: u64,
       events_seen: ParamHandle,   // счётчик для тестов и dev-вывода
   }

   impl MidiPlayer {
       pub fn new(schedule: Schedule, events_seen: ParamHandle) -> Self;
   }
   ```

   - в начале `process` узел проверяет `ctx.position < last_position`
     (транспорт ушёл назад) и тогда делает `cursor = schedule.cursor()`;
     затем `last_position = ctx.position`;
   - дальше в цикле берёт `next_event(&mut cursor, ctx.position,
     ctx.frames)`, пока не вернётся `None`, и на каждое событие делает
     `events_seen.set(events_seen.value() + 1.0)` (счётчик — параметр из
     SPEC-006, без сглаживания: `smoothing_ms == 0`);
   - `NoteOn`/`NoteOff` только считаются, звук не генерируется (синтезатора
     нет);
   - в `output` узел **ничего не пишет** (на выходе тишина); это источник
     событий, который проверяется тестом, а не инструмент.

8. **Если событие вышло за квант** — оно не теряется: `next_event` просто
   не вернёт его в этом кванте, `cursor` остаётся на нём.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| `ppqn == 0` | `ScheduleError::InvalidPpqn` |
| `sample_rate == 0` | `ScheduleError::InvalidSampleRate` |
| `micros_per_quarter == 0` | `ScheduleError::InvalidTempo { tick }` |
| файл только с `Tempo`, без нот | `ScheduleError::Empty` |
| событие ровно на `position + frames` | **не** попадает в этот квант (диапазон полуоткрытый) |
| событие ровно на `position` | попадает в этот квант |
| `frames == 0` и событие на `position` | не возвращается: диапазон пуст |
| `Seek` вперёд мимо событий | они пропускаются, курсор сдвигается |
| `Seek` назад | события до новой позиции проиграны: курсор уже за ними, `next_event` вернёт `None`, пока не дойдёт до позиции |
| два события на одном сэмпле | оба возвращаются, порядок = порядок в `ScheduleSpec` |
| `from_midi` с `tail_samples = 0` | `length_samples` = позиция последнего события |
| огромный тик (`u64::MAX / 4`) | `sample` считается через `f64` и клампится в `u64::MAX`; `length_samples` не переполняется, ошибки нет |
| `tail_samples` при пустом списке событий | `ScheduleError::Empty` (длина не считается) |

## Критерии приёмки

- [ ] Формула тик → сэмпл протестирована отдельно: 120 bpm, 480 ppqn,
      48 кГц → тик 480 = одна четверть = 24 000 сэмплов; тик 0 = сэмпл 0.
- [ ] Тест: смена темпа посередине (120 → 60 bpm) даёт скачок сэмплов
      ровно в отношении `2:1` после `Tempo`-события.
- [ ] Тест: `next_event` уважает полуоткрытый диапазон (`position + frames`
      не входит).
- [ ] Тест: `frames == 0` ничего не возвращает и не сдвигает курсор.
- [ ] Тест: `Seek` вперёд — пропущенные события не возвращаются; `Seek`
      назад — `MidiPlayer` сбрасывает курсор и события проигрываются снова,
      порядок монотонный.
- [ ] Тест: `next_event` на 100 000 событий и 1000 квантов — детерминизм
      бит-в-бит (счётчик `events_seen` совпадает от прогона к прогону).
- [ ] Тест на `rg`-уровне (ревью): в `schedule.rs` в `next_event` нет
      `Vec::push`, `Box::new`, `format!`, `println!`, `Mutex`, `RwLock`.
- [ ] `daw_app::nodes::MidiPlayer` использует `Schedule`, не звучит
      (тишина) и считает события через `ParamHandle`.
- [ ] `docs/MIDI.md` получает раздел «Планировщик» с формулой тик → сэмпл.
- [ ] `docs/REALTIME.md` получает раздел «Опрос расписания» со ссылкой.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --all-targets --locked` зелёные.

## Вне области

- Синтез звука по нотам (осцилляторы, огибающие, полифония).
- Автоматизация параметров из MIDI.
- Запись MIDI (SMF-writer).
- MIDI 2.0, SysEx, RPN/NRPN.
- Изменение размера (`TimeSignature`).
- Ресемплинг подготовки расписания при смене `sample_rate` (расписание
  пересобирается целиком).
- Любая правка `crates/audio-core/tests/golden/*.wav`.
