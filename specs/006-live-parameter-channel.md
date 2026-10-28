# SPEC-006: Живой канал параметров (атомики, без блокировок)

Статус: ready
Владелец: команда движка
Для агента: разрешено реализовывать

Файлы: `crates/audio-core/src/params.rs` (новый модуль),
`crates/audio-core/src/{lib.rs,graph.rs}`, `crates/daw-app/src/{nodes.rs,lib.rs}`,
`crates/audio-core/tests/`, `docs/REALTIME.md`, `docs/PARAMS.md`

Разрешённый рефакторинг: `daw_app::nodes::Tone` получает управляемый
`level` через `ParamHandle` (см. п. 5). Публичный вид `Tone` меняется: поле
`level` заменяется конструктором `Tone::new(freq_hz, level)`.

## Проблема

Сейчас узел узнаёт об изменении звука только так: UI останавливает звук,
блокирует контрольный поток и через `CommandConsumer`/`&mut Graph` меняет
структуру или создаёт новый граф. Для ручки громкости или частоты это
неприемлемо: переключение на 1 квант рвёт звук, а `TransportCommand` уже
описывает только транспорт, не параметры узлов.

Нужен второй, отдельный канал **значений**: производитель (UI/агент) пишет
число, аудиопоток читает его раз в квант. Никаких блокировок, никаких
аллокаций, никаких команд — только атомики.

## Поведение

1. Новый модуль `audio-core/src/params.rs`, публично доступный как
   `audio_core::params` и реэкспортированный из корня (`ParamId`,
   `ParamHandle`, `ParamRegistration`, `ParamSpec`, `ParamError`).
   Конструктор — `ParamHandle::new` (см. п. 2).

2. Параметр — гладкое моно-значение `f32` в `Arc`, разделяемое между
   контрольным потоком и узлом:

   ```rust
   impl ParamHandle {
       pub fn new(spec: ParamSpec) -> Self   // контрольный поток
       pub fn spec(&self) -> ParamSpec
       pub fn value(&self) -> f32            // текущий target
       pub fn set(&self, value: f32)         // контрольный поток
       pub fn next(&self, step_frames: usize, sample_rate: u32) -> f32
   }
   ```

   `set` — из UI; узлу нужен только `next`. Ошибки конфигурации ловятся в
   `ParamSpec::new`, поэтому `ParamHandle::new` не возвращает `Result`.

3. `ParamSpec` — plain-data описание:

   ```rust
   ParamSpec { id: ParamId, name: &'static str, min: f32, max: f32,
               default: f32, smoothing_ms: u32 }
   ParamSpec::new(...) -> Result<ParamSpec, ParamError>
   ParamSpec::unsmoothed(...) -> Result<ParamSpec, ParamError>   // smoothing_ms = 0
   ```

   `ParamId` — `u32` newtype, уникальный в графе (см. п. 6).

   `ParamError`: `InvalidRange { min, max }` (пустой/перевёрнутый диапазон,
   `NaN`, `±∞`), `DefaultOutOfRange { default, min, max }`,
   `SmoothingTooLong { ms, max }`, `DuplicateId { id }`, `UnknownParam { id }`,
   `RegistryFull { max }`.

4. **Сглаживание.** `ParamHandle::next(&self, step_frames: usize, sample_rate: u32) -> f32`
   возвращает значение на *начало* следующего кванта, продвигая внутренний
   ramp к target:

   - если `smoothing_ms == 0`, всегда возвращает `value()` (target);
   - иначе линейный ramp на `ceil(smoothing_ms * sample_rate / 1000)`
     сэмплов от значения в момент `set` до target; за один вызов `next`
     ramp продвигается на `step_frames`. При штатной работе `step_frames`
     равен длине кванта, поэтому ramp, начатый в `set`, доходит до цели
     примерно за `smoothing_ms` мс; если кванты короче, ramp просто идёт
     столько же сэмплов и потому дольше по времени — это допустимо и
     отдельно не ограничивается;
   - по достижении цели ramp «защёлкивается» на target, дальнейшие
     `next` не двигают ramp и возвращают target;
   - ramp перезапускается только при фактической смене target: `set`
     тем же значением ничего не меняет, а `set` новым — начинает ramp
     от **текущего сглаженного** значения, а не от старта.

   Формальная спецификация (реализация обязана её повторить дословно;
   `from`, `left`, `a` — `f32`, кроме `a` == 0.0 ramp вырождается в
   мгновенный переход):

   ```text
   a = smoothing_ms * sample_rate / 1000     // длина ramp в сэмплах (float)
   if a == 0.0 { return target }             // без сглаживания
   left = max(left - step_frames as f32, 0.0)
   if left > 0.0 { return from + (target - from) * (1.0 - left / a) }
   return target                             // left == 0: цель достигнута
   ```

   `a`, `from`, `left` живут в `ParamInner`; `reset_for_test` не нужен.
   `sample_rate` берётся из `ctx.sample_rate` при каждом вызове — узлу он
   и так доступен, а хранить копию значило бы поддерживать смену частоты
   посреди ramp («Вне области»).

   `set(v)` (контрольный поток): `v = clamp(v, min, max)`; `NaN` → ничего
   не меняется; если `v == target` — выйти, не трогая `from`/`left`; иначе
   `from` = текущее сглаженное значение (`left > 0` → формула выше, иначе
   `target`), `target = v`, `left = a` (если `a > 0`, иначе просто
   `from = target = v`). Отсюда первый `next` после `set` возвращает `from` — ramp
   ещё не сдвинулся. `a` пересчитывается на каждом `next` из `smoothing_ms`
   и `sample_rate`; нигде не кэшируется.

   `a > 0` означает `smoothing_ms * sample_rate / 1000 > 0`, то есть
   проверять надо именно это, а не `smoothing_ms == 0`: при очень коротком
   ramp и малой частоте возможно усечение до нуля.

   `ParamHandle` — дешёвая разделяемая ссылка на параметр; в доке пишем
   прямо, что это `Arc`, чтобы читатель видел цену клонирования:

   ```rust
   pub type ParamHandle = Arc<ParamInner>;   // Clone, не Copy
   ```

   `Arc::clone` допустим **только** в конструкторе узла и в контрольном
   потоке: в узле лежит уже готовый handle, `next` работает через `&self`.
   `next` не аллоцирует, не блокирует, не паникует и не клонирует `Arc`.
   Отдельного типа `Param` нет — одно имя на весь API. `unsafe` не нужен.

5. **Узлы.** Второй метод в `NodeProcessor` **не добавляется**, и
   `ProcessContext` не меняется (он остаётся `Copy`). Узел сам хранит
   `ParamHandle` и вызывает `next(ctx.frames, ctx.sample_rate)` в начале
   своего `process`:

   ```rust
   pub struct Tone { freq_hz: f64, level: ParamHandle }
   impl Tone {
       pub fn new(freq_hz: f64, level: ParamHandle) -> Self { /* ... */ }
   }
   impl NodeProcessor for Tone {
       fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
           let level = self.level.next(ctx.frames, ctx.sample_rate);
           // ... dst[i] = sample * level * gain_i ...
       }
   }
   ```

   Почему так: граф не маршрутизирует значения, а параметру не нужна
   позиция/транспорт; расширение трейта заставило бы каждого автора узла
   возвращать «пустой» handle. `daw_app::nodes::Tone` переделывается на
   `Tone::new(440.0, ParamHandle::new(ParamSpec::new(ParamId(0), "level", 0.0,
   1.0, 0.5, 10)?))`; `build_demo_graph` регистрирует параметр в графе.

6. **Реестр параметров в графе.** `Graph` умеет собрать список параметров для
   UI (контрольный поток, не аудио):

   ```rust
   Graph::params(&self) -> &[ParamRegistration]   // порядок = порядок register_param
   Graph::param(&self, id: ParamId) -> Result<ParamHandle, ParamError>
   Graph::register_param(&mut self, handle: ParamHandle) -> Result<(), ParamError>
   ```

   `ParamRegistration { spec: ParamSpec }` — только для чтения и
   перечисления (id и name берутся из `spec`).
   `add_boxed` **не** автоматически регистрирует параметры: реестр —
   явный вызов `register_param` в контрольном потоке. Дубликат id →
   `ParamError::DuplicateId`. Ёмкость реестра резервируется в
   `Graph::with_capacity` (см. `max_params`), превышение →
   `ParamError::RegistryFull` — **не** аллокация в горячем пути.

   **Отдельное решение:** сигнатура `Graph::with_capacity` расширяется до
   `with_capacity(sample_rate, channels, max_quantum, max_nodes, max_params)`.
   Обновите все места вызова (тесты, `daw-app`).

7. **Тишина при остановке.** Параметры не обнуляются при `Stop`; они — часть
   состояния инструмента, а транспорт — часть часов. Тест фиксирует, что
   `level` сохраняется через stop/play.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| `min > max` | `ParamError::InvalidRange` |
| `min == max` | допустимо (константа), `set` игнорирует всё, кроме этого значения |
| `default` вне `[min, max]` | `ParamError::DefaultOutOfRange` |
| `smoothing_ms > 10_000` | `ParamError::SmoothingTooLong { ms, max: 10_000 }` |
| `set(NaN)` | игнорируется, значение не меняется |
| `set(1e9)` при `max = 1.0` | защёлкивается на `max` |
| `set` тем же значением во время ramp | ramp не перезапускается: `from`/`left` не меняются |
| `set` новым значением во время ramp | ramp начинается от текущего сглаженного, `left = a` |
| `smoothing_ms == 0` (`a == 0`) | `next` возвращает target сразу |
| `a < step_frames` (ramp короче кванта) | `left` уменьшается на `step_frames`, но не ниже нуля: за один `next` цель достигается без перелёта |
| `step_frames == 0` | возвращает значение в текущей точке ramp, `left` не меняется |
| `min`/`max` == `NaN`/`±∞` | `ParamError::InvalidRange` |
| `Graph::param` с неизвестным id | `ParamError::UnknownParam { id }` |
| `register_param` дважды | `ParamError::DuplicateId { id }` |
| больше `max_params` | `ParamError::RegistryFull` |
| `Stop` → `Play` | значение параметра сохранено |
| `max_params == 0` в `Graph::with_capacity` | допустимо, любой `register_param` → `ParamError::RegistryFull` |

## Критерии приёмки

- [ ] Длина ramp считается из `sample_rate` на каждом `next`; тест фиксирует
      первый `next` после `set` == значению в момент `set`.
- [ ] Юнит-тесты `params.rs` на каждую строку «Поведения» и «Граничных случаев».
- [ ] Тест на `rg`-уровне (или ревью): в `params.rs` нет `Mutex`, `RwLock`,
      `Vec::push`, `format!`, `Box::new`, `println!`.
- [ ] Интеграционный тест `crates/audio-core/tests/params.rs`: два кванта подряд
      со сменой `level` посередине дают **разные** значения, а ramp
      детерминирован (бит-в-бит при повторном прогоне).
- [ ] Тест: `set` **тем же** значением во время ramp → ramp не
      перезапускается (монотонность сохраняется, `left` убывает как без
      второго `set`); `set` новым значением → ramp начинается от текущего
      сглаженного, без скачка к старому target.
- [ ] Тест: `smoothing_ms == 0` → значение меняется на границе кванта скачком
      (без интерполяции).
- [ ] Тест: `AudioBuffer`/`Renderer` не изменились по поведению — существующие
      goldens проходят без перегенерации.
- [ ] `daw_app::nodes::Tone` использует параметр; `daw bounce` и `daw play`
      звучат как раньше (амплитуда 0.5, 440 Гц) — golden не меняется.
- [ ] `docs/PARAMS.md` (новый) описывает два канала: команды и параметры, и
      почему параметры — атомики, а не `rtrb`.
- [ ] `docs/REALTIME.md` получает раздел «Параметры» со ссылкой на `PARAMS.md`.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --all-targets --locked` зелёные.

## Вне области

- Автоматизация параметров (огибающие, запись движения ручки).
- MIDI-CC → параметр.
- Модуляция параметров LFO/макросами.
- Сериализация реестра параметров в проект.
- Полифония и голосовой менеджер.
- Смена `sample_rate` посреди ramp (ramp не пересчитывается на лету).
- Многопоточная запись `set` из нескольких потоков одновременно: API
  рассчитан на один производитель, `set` атомарен, но порядок двух
  одновременных `set` не определён.
- Любая правка `crates/audio-core/tests/golden/*.wav`.
