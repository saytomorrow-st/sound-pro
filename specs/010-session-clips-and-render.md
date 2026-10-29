# SPEC-010: Клипы сессии — треки, обрезка и микс в графе

Статус: ready
Владелец: команда движка
Для агента: разрешено реализовывать

Файлы: `crates/audio-core/src/session.rs` (новый модуль),
`crates/audio-core/src/lib.rs`, `crates/audio-core/src/project.rs` (чтение и
запись нового поля), `crates/daw-app/src/nodes.rs`
(`ClipSource`/`Track`/`Strip`), `crates/daw-app/src/lib.rs`
(`build_session_graph`, `render_session`), `crates/daw-app/src/main.rs`
(подкоманды `render-session` и `session show`),
`crates/audio-core/tests/session.rs` (новый),
`crates/audio-core/tests/project.rs` (дополнить),
`crates/daw-app/tests/session.rs` (новый), `docs/SESSION.md` (новый),
`README.md`

## Проблема

Сейчас в графе живёт ровно один источник звука: демо-«tone» или один
MIDI-файл целиком через `daw render-midi`. Понятия «трек», «клип на таймлайне»
и «границы клипа внутри трека» нет, поэтому нельзя ни положить два файла на
разные дорожки со сдвигом, ни обрезать начало, ни увидеть, как это выглядит в
проекте. Без этого следующий шаг (редактор) упирается в заглушку.

## Поведение

### 1. Модель сессии — `audio-core/src/session.rs`

Публичная как `audio_core::session` и реэкспортированная из корня (`Clip`,
`Track`, `Session`, `SessionError`). Только данные: ни графа, ни I/O, ни
времени.

```rust
pub struct Clip {
    pub start_samples: u64, // начало клипа на таймлайне
    pub length_samples: u64, // видимая длина, > 0
    pub offset_samples: u64, // сдвиг внутри содержимого (обрезка слева)
    pub source_length: u64,  // длина содержимого целиком, > 0
    pub gain: f32,           // 0.0..=1.0
    pub name: String,        // ≤ 128 байт, может быть пустым
}

pub struct Track {
    pub name: String,         // ≤ 128 байт
    pub gain: f32,            // 0.0..=1.0
    pub mute: bool,
    pub clips: Vec<Clip>,
}

pub struct Session {
    pub name: String,
    pub sample_rate: u32,
    pub tempo_bpm: f64,
    pub tracks: Vec<Track>,
}
```

`Session` вкладывает `Vec<Track>`, а не `ParamState`: у клипов есть своя
ценность (позиция, обрезка), и переиспользовать `Project` как есть нельзя без
того, чтобы `Project` потерял смысл «снимок параметров».

### 2. Правила валидации — одни и те же для конструктора модели и загрузки

`Session::validate(&self) -> Result<(), SessionError>` проверяет:

- `sample_rate == 0` → `InvalidSampleRate`;
- `tempo_bpm <= 0` или не конечный → `InvalidTempo`;
- имя сессии/трека/клипа длиннее 128 байт → `NameTooLong`;
- `length_samples == 0` → `EmptyClip { track, clip }`;
- `source_length == 0` → `EmptySource { track, clip }`;
- `offset_samples + length_samples > source_length` →
  `ClipOutOfSource { track, clip }` (клип не может звучать за пределами
  содержимого);
- `start_samples + length_samples` переполняет `u64` →
  `TimelineOverflow { track, clip }`;
- `gain` не конечный или вне `[0, 1]` → `InvalidGain { track }`
  (для клипа — `InvalidClipGain { track, clip }`);
- больше 32 треков → `TooManyTracks { max: 32 }`;
- больше 128 клипов на трек → `TooManyClips { track, max: 128 }`.

Индексы `track`/`clip` — позиции в `Vec`, с нуля.

`Session::length_samples()` — `max(start + length)` по всем клипам, `0` для
пустой сессии. `Session::track_length_samples(track)` — то же по одному треку.

### 3. Узлы — `daw-app/src/nodes.rs`

```rust
/// Источник PCM-содержимого клипа: сэмплы на канал, уже на частоте сессии.
pub trait ClipSource {
    fn frames(&self) -> u64;            // = source_length
    fn channel(&self, ch: usize) -> &[f32];
}
```

- `ClipSource` реализуется `Vec<f32>`-обёрткой `MonoClip` и (в тестах) любым
  массивом; интерфейс только чтения, поэтому узел не может испортить
  содержимое.
- `Track` — `NodeProcessor` с обрезкой по таймлайну. Для каждой позиции
  `p` в кванте:
  - если `p` попадает в `[start, start + length)` клипа, сэмпл берётся по
    индексу `offset + (p - start)`, умножается на `gain` клипа;
  - иначе тишина — и **тишина, а не пропуск счёта**: клип за пределами своей
    длины не звучит, даже если содержимое длиннее видимой части;
  - клипы трека суммируются, затем умножаются на `gain` трека; `mute`
    обнуляет весь трек;
  - никаких аллокаций, блокировок и `unwrap` — только арифметика по
    заранее подготовленным срезам.
- `Strip { source, offset, gain }` — узел-обработчик одного загруженного
  источника с известным сдвигом; используется как минимальный вертикальный
  срез для проверки суммирования в графе.

### 4. Сборка графа — `daw-app/src/lib.rs`

```rust
pub struct TrackSource {
    pub track: audio_core::session::Track,
    pub clips: Vec<Box<dyn ClipSource + Send>>, // по одному на клип, в том же порядке
}

pub struct SessionJob {
    pub session: audio_core::session::Session,
    pub tracks: Vec<TrackSource>,
    pub sample_rate: u32,
    pub block_frames: usize,
    pub frames: Option<u64>, // иначе Session::length_samples
}

pub fn build_session_graph(...) -> Result<Graph, Box<dyn Error>>;
pub fn render_session(job: &SessionJob) -> Result<SessionReport, Box<dyn Error>>;
```

- Каждый трек — один узел графа; результаты складываются в `mix`
  (граф и так суммирует, см. `Graph::process`). Порядок треков = порядок
  узлов.
- `frames` по умолчанию — `Session::length_samples()`; явное значение
  больше длины сессии дополняет хвост тишиной, меньше — обрезает.
- `SessionReport { frames, session_frames, tracks }`, где `tracks` — число
  смонтированных узлов-треков.
- Ошибки: `session.validate()` → `SessionError`; число треков в
  `TrackSource` не совпадает с `session.tracks` → `TrackCountMismatch`;
  у трека не совпадает число источников и клипов → `ClipCountMismatch`.
  Обе ошибки — `daw-app`-овские, в `audio-core` их нет.
- `render_session` пишет mono WAV через `OfflineRenderer`, как
  `render_midi`, и возвращает отчёт.

### 5. Проект хранит сессию — `audio-core/src/project.rs`

1. В `Project` появляется поле `session: Session` (сериализуется последним,
   под ключом `"session"`). При `capture` оно равно
   `Session::new_untitled(sample_rate, tempo_bpm)` — пустая сессия с тем же
   именем, что и проект; при этом `Project::capture` остаётся чистым
   проходом по `Graph::params()`.
2. Новые методы:
   ```rust
   impl Project {
       /// Документ с явной сессией.
       pub fn with_session(project: Project, session: Session) -> Project;
       pub fn session(&self) -> &Session;
   }
   ```
3. Загрузка: `session` валидируется через `Session::validate`; ошибки
   заворачиваются в `ProjectError::Session(SessionError)`, текст ошибки —
   исходный. Отсутствующий ключ `"session"` → `MissingField { field:
   "session" }`, как и для остальных полей.
4. Обратная совместимость **не нужна**: версии формата до первого релиза не
   поддерживаются (SPEC-008, «Вне области»), но в `docs/PROJECT.md`
   добавляется пример с сессией.

### 6. CLI

```
daw session show <path>                 # проект со вложенной сессией
daw render-session <out.wav> [--seconds N]
```

- `session show` читает проект и печатает строку на трек
  (`<index> <name> gain=<gain> mute=<bool> clips=<n>`), а после — строку на
  клип (`  <clip> start=<start> length=<length> offset=<offset>`), затем
  `session_frames=<n>`. Ошибка чтения/разбора → stderr и код выхода 1;
  отсутствие аргумента → код выхода 1.
- `render-session` собирает демо-сессию **в памяти** (два трека, по одному
  сгенерированному синусу: 440 Гц на 0.5 с со сдвигом 0 и 660 Гц на 0.25 с со
  сдвигом 0.25 с), рендерит и печатает
  `rendered <frames> frames (<session_frames> session), <tracks> track(s)`.
  `--seconds` переопределяет длину. Ошибки → stderr, код выхода 1.
- Генерация синусов живёт в `daw-app` (`build_demo_session`), не в
  `audio-core`.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| пустая сессия (0 треков) | `length_samples() == 0`, граф из 0 узлов рендерит тишину |
| `frames` больше длины сессии | хвост — точная тишина (`0.0`) |
| `frames` меньше длины сессии | рендер обрезан, ошибки нет |
| клип на весь источник (`offset = 0`, `length = source`) | сэмплы совпадают с источником побитово |
| обрезка справа (`length < source - offset`) | после `start + length` — тишина, содержимое за границей не звучит |
| обрезка слева (`offset > 0`) | первый сэмпл клипа = `source[offset]` |
| два клипа подряд без зазора | сумма на стыке, без щелчка-обрыва длины |
| два клипа перекрываются | суммируются по сэмплам (заменено SPEC-038: верхний клип перекрывает нижний с кроссфейдом) |
| `mute = true` | весь трек — тишина, остальные не меняются |
| `gain = 0` у клипа | клип молчит, остальные клипы трека на месте |
| `offset + length > source_length` | `ClipOutOfSource` |
| `length = 0` | `EmptyClip` |
| `gain = 1.5` | `InvalidClipGain` |
| 33 трека | `TooManyTracks { max: 32 }` |
| 129 клипов на треке | `TooManyClips { track, max: 128 }` |
| `start = u64::MAX`, `length = 1` | `TimelineOverflow` |
| `TrackSource` не совпадает по числу с сессией | `TrackCountMismatch` |
| в треке 2 клипа, 1 источник | `ClipCountMismatch` |
| `session` в JSON отсутствует | `MissingField { field: "session" }` |
| неизвестное поле внутри `session` | `UnknownField { field }` |
| `Project::with_session` + round-trip | сессия сохраняется (`PartialEq`) |

## Критерии приёмки

- [ ] Юнит-тесты на каждое правило валидации и на `length_samples`.
- [ ] Рендер полного клипа совпадает с источником побитово.
- [ ] Обрезка слева и справа даёт ожидаемые срезы (первый сэмпл, тишина за
      границей).
- [ ] Два трека со сдвигом дают сумму, проверенную по сэмплам в нескольких
      точках.
- [ ] `mute` и `gain` проверены отдельными тестами.
- [ ] Граф детерминирован: два прогона — побайтово равные WAV.
- [ ] Ошибки `TrackCountMismatch`/`ClipCountMismatch` покрыты тестами.
- [ ] Round-trip проекта с непустой сессией даёт равный `Project`.
- [ ] `daw session show` и `daw render-session` работают на реальном файле;
      `session show` без аргумента и с несуществующим файлом → код выхода 1.
- [ ] `rg` подтверждает: в `audio-core/src/session.rs` нет `std::fs`,
      `std::env`, `println!`, `std::time`, `unwrap`; в `Track::process` нет
      аллокаций и блокировок.
- [ ] `docs/SESSION.md` с моделью, примером JSON и описанием CLI; ссылка из
      `README.md`.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --all-targets --locked` зелёные.

## Вне области

- Загрузка аудиофайлов с диска (источники клипов создаются в памяти;
  декодирование WAV — отдельная спецификация).
- Перемещение/редактирование клипов в UI, undo, щелчок-выравнивание,
  фейды и кроссфейды между клипами.
- Трековые эффекты, шины, сайдчейн, автоматизация.
- Изменение частоты дискретизации клипов (считается, что содержимое уже на
  частоте сессии).
- Полифония клипов сверх 128 на трек и треков сверх 32.
- Любая правка `crates/audio-core/tests/golden/*.wav` и ослабление тестов.
