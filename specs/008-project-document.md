# SPEC-008: Документ проекта (сохранение/загрузка, JSON)

Статус: ready
Владелец: команда движка
Для агента: разрешено реализовывать

Файлы: `crates/audio-core/src/project.rs` (новый модуль),
`crates/audio-core/src/lib.rs`, `crates/daw-app/src/main.rs`,
`crates/daw-app/src/lib.rs`, `crates/audio-core/tests/project.rs` (новый),
`docs/PROJECT.md` (новый), `README.md`

## Проблема

Сейчас сессия существует только в памяти: параметры, темп и размер заданы
константами в `build_demo_graph`. Закрыл приложение — потерял работу. Нужен
персистентный документ: что за проект, какой темп, какие параметры и с
какими значениями.

Сохранять надо **описание сессии**, а не узлы: `NodeProcessor` — трейт с
замыканиями и `Box<dyn …>`, сериализовать его нельзя, а восстанавливать
произвольный код из файла небезопасно.

## Поведение

1. Новый модуль `audio-core/src/project.rs`, публичный как
   `audio_core::project` и реэкспортированный из корня (`Project`,
   `ProjectError`).

2. Типы (все — plain data, `PartialEq` для тестов):

   ```rust
   pub struct Project {
       pub name: String,          // отображаемое имя, ≤ 128 байт
       pub sample_rate: u32,      // > 0
       pub tempo_bpm: f64,        // > 0 и конечное
       pub params: Vec<ParamState>,
   }

   pub struct ParamState {
       pub id: u32,               // ParamId
       pub name: String,          // ≤ 128 байт
       pub min: f32,
       pub max: f32,
       pub default: f32,
       pub smoothing_ms: u32,
       pub value: f32,            // текущее значение, в [min, max]
   }
   ```

3. **Формат — JSON, руками, без зависимостей.** `audio-core` не тянет
   `serde` (правило AGENTS.md: минимум зависимостей в движке), а формат
   должен читаться человеком и диффиться в git. Поэтому пишем и читаем
   собственный минимальный кодек:
   - поддерживаются только объекты, массивы, строки, числа и `true/false`;
   - **никаких** `null`, вложенности глубже 8 уровней, `\u`-escape и чисел с
     экспонентой: такой вход → `ProjectError::Unsupported { at }`;
   - парсер работает по байтам `&[u8]`, не паникует ни на каком входе и
     возвращает `ProjectError::Malformed { at }` с байтовым смещением
     первого нарушения.

4. **Сериализация детерминирована**: порядок ключей фиксирован порядком
   объявления полей, `params` — в порядке `Vec`, отступ 2 пробела, `\n` в
   конце файла. Один и тот же `Project` всегда даёт побайтово один и тот же
   текст.

   ```rust
   impl Project {
       pub fn to_json(&self) -> String;                       // контрольный поток
       pub fn from_json(text: &str) -> Result<Project, ProjectError>;
   }
   ```

5. **Валидация при загрузке** (после разбора, до возврата):
   - `sample_rate == 0` → `ProjectError::InvalidSampleRate`;
   - `tempo_bpm` не конечный или `<= 0` → `ProjectError::InvalidTempo`;
   - `name` или `ParamState::name` длиннее 128 байт → `ProjectError::NameTooLong`;
   - `min`/`max`/`default`/`value` не конечные, `min > max` или `value` вне
     `[min, max]` → `ProjectError::InvalidParam { id }`;
   - дубликаты `id` в `params` → `ProjectError::DuplicateParam { id }`;
   - пропущенный ключ → `ProjectError::MissingField { field }`;
   - лишний ключ → `ProjectError::UnknownField { field }` (опечатка в
     файле должна быть ошибкой, а не тишиной).

6. **Связь с графом** — только вниз, из проекта в живые объекты:

   ```rust
   impl Project {
       /// Применяет сохранённые значения к уже зарегистрированным параметрам.
       pub fn apply_to(&self, graph: &Graph) -> Result<(), ProjectError>;

       /// Снимает снимок параметров графа (имена и спеки — из реестра,
       /// значения — текущие).
       pub fn capture(name: &str, sample_rate: u32, tempo_bpm: f64, graph: &Graph) -> Project;
   }
   ```

   `apply_to` ищет параметр через `Graph::param`; если параметра нет в
   графе — `ProjectError::UnknownParam { id }` (проект ссылается на то, чего
   в этой сборке нет). `capture` — чистый проход по `Graph::params()`, без
   захвата чего-либо ещё.

7. **CLI.** Две команды в `daw`:
   - `daw project save <path>` — собирает демо-граф, меняет `tone.level` на
     0.25 и пишет проект в файл; печатает путь и число параметров;
   - `daw project show <path>` — читает файл и печатает `name`, `sample_rate`,
     `tempo_bpm` и по строке на параметр (`<id> <name> = <value>`);
   - ошибка чтения/разбора → сообщение в stderr и код выхода 1;
   - запись — только в контрольном потоке (в `main`, не в аудиоколбэке).

8. **I/O живёт в `daw-app`.** `audio-core` умеет только текст ↔ структура;
   файлы открывает оболочка (`std::fs::write`/`read_to_string` в `main.rs`).
   В `project.rs` нет `std::fs`, `std::env`, `println!`.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| пустая строка | `ProjectError::Malformed { at: 0 }` |
| `{}` | `MissingField { field: "name" }` |
| лишний ключ `tempoBPM` (опечатка) | `UnknownField { field: "tempoBPM" }` |
| `null` где угодно | `Unsupported { at }` |
| число с экспонентой `1e3` | `Unsupported { at }` |
| вложенность 9 уровней | `Unsupported { at }` |
| строка без закрывающей кавычки | `Malformed { at }` |
| отрицательный `sample_rate` | `Malformed { at }` (число не влезает в `u32`) |
| `value` вне `[min, max]` | `InvalidParam { id }` |
| два параметра с `id = 3` | `DuplicateParam { id: 3 }` |
| `tempo_bpm = 0` | `InvalidTempo` |
| `\uXXXX` в строке | `Unsupported { at }` |
| `capture` → `to_json` → `from_json` | тот же `Project` (round-trip, `PartialEq`) |
| `apply_to` с параметром, которого нет в графе | `UnknownParam { id }` |
| очень длинный файл (1 МБ мусора) | `Err`, без паники, < 1 с |

## Критерии приёмки

- [ ] Round-trip: `capture` → `to_json` → `from_json` даёт равный `Project`
      на демо-графе и на проекте с 64 параметрами.
- [ ] `to_json` детерминирован: два вызова на одном `Project` дают
      побайтово равные строки; порядок ключей зафиксирован тестом на
      подстроки.
- [ ] `from_json` на 1 МБ случайных байт возвращает `Err`, не паникует,
      укладывается в 1 с (детерминированный xorshift, как в `tests/midi.rs`).
- [ ] Каждая строка таблицы граничных случаев покрыта тестом.
- [ ] `apply_to` меняет живые параметры (проверяется через
      `ParamHandle::value` после загрузки).
- [ ] `daw project save` / `daw project show` работают на реальном файле;
      `show` без аргумента и с несуществующим файлом дают код выхода 1.
- [ ] `rg` подтверждает: в `crates/audio-core/src/project.rs` нет `std::fs`,
      `std::env`, `println!`, `unwrap`/`expect` на данных из файла.
- [ ] `docs/PROJECT.md` с форматом и примером; ссылка из `README.md`.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --all-targets --locked` зелёные.

## Вне области

- Сохранение узлов и их топологии (только параметры).
- Несколько треков, клипы, автоматизация, MIDI-файлы внутри проекта.
- Бинарный формат, сжатие, миграции версий формата (поле версии не
  добавляем: до первого релиза формат меняется свободно).
- `serde` и любые новые зависимости.
- Автосохранение и undo.
- Любая правка `crates/audio-core/tests/golden/*.wav`.
