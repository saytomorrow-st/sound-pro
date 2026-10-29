# Документ проекта (SPEC-008)

Сессия сохраняется как **текстовый JSON**. Ни `serde`, ни бинарного формата:
файл должен открываться в редакторе и диффоваться в git, а разбор должен
падать громко, если в файле опечатка.

## Формат

```json
{
  "name": "demo",
  "sample_rate": 48000,
  "tempo_bpm": 120,
  "params": [
    {
      "id": 0,
      "name": "tone.level",
      "min": 0,
      "max": 1,
      "default": 0.5,
      "smoothing_ms": 10,
      "value": 0.25
    }
  ]
}
```

| Поле | Тип | Смысл |
|---|---|---|
| `name` | строка ≤ 128 байт | имя сессии |
| `sample_rate` | `u32 > 0` | частота, на которой сессия писалась |
| `tempo_bpm` | `f64 > 0` | темп |
| `params[]` | массив | параметры в порядке сессии |
| `params[].id` | `u32` | `ParamId`; уникален внутри файла |
| `params[].name` | строка ≤ 128 байт | имя для UI (в рантайме это `&'static str`, в файле — `String`) |
| `params[].min`/`max`/`default` | `f32` | спецификация параметра на момент сохранения |
| `params[].smoothing_ms` | `u32` | длина рампы, `0` — прыжок |
| `params[].value` | `f32` | сохранённое значение, `min ≤ value ≤ max` |

Порядок ключей фиксирован декларацией (см. `Project::to_json`), поэтому два
сохранения одного состояния дают **побайтово равный** файл.

## Что кодек не принимает

Формат намеренно уже JSON: то, что может быть опечаткой, отвергается с
позицией в байтах, а не угадывается.

| Вход | Ответ |
|---|---|
| `null` в любом месте | `Unsupported { at }` |
| числа с экспонентой `1e3` | `Unsupported { at }` |
| `\uXXXX` в строке | `Unsupported { at }` |
| вложенность глубже 8 уровней | `Unsupported { at }` |
| лишний/неизвестный ключ | `UnknownField { field }` |
| пропущенный ключ | `MissingField { field }` |
| `sample_rate = 0` | `InvalidSampleRate` |
| `tempo_bpm ≤ 0` или `NaN`/`inf` | `InvalidTempo` |
| `min > max`, `default`/`value` вне `[min, max]`, не конечное число | `InvalidParam { id }` |
| два параметра с одним `id` | `DuplicateParam { id }` |
| имя длиннее 128 байт | `NameTooLong` |
| битый JSON | `Malformed { at }` |

Числа пишутся через `Display`, то есть в кратчайшей точной форме без
экспоненты: запись и разбор — обратные операции, включая субнормальные
значения. Этот инвариант закреплён тестом
`awkward_floats_survive_the_text_format_exactly`.

## Связь с живым графом

```text
Graph::params() ──capture──► Project ──to_json──► текст ──► std::fs::write
Graph::param()  ◄──apply_to── Project ◄──from_json── текст ◄── std::fs::read_to_string
```

- `capture` — чистый проход по реестру: имена и спеки берутся из
  `ParamRegistration`, значения — из `ParamHandle::value()`.
- `apply_to` сначала ищет **все** `id` через `Graph::param`, и только потом
  пишет значения. Документ, в котором одного параметра нет в этой сборке,
  возвращает `UnknownParam { id }`, не изменив ни одного значения.
- `audio-core` не знает про файлы: `std::fs` есть только в `daw-app/src/main.rs`.

## CLI

```bash
cargo run -q --bin daw -- project save /tmp/demo.json
cargo run -q --bin daw -- project show /tmp/demo.json
```

```text
saved 1 parameter(s) to /tmp/demo.json
name: demo
sample_rate: 48000
tempo_bpm: 120
0 tone.level = 0.25
```

`save` собирает демо-граф, ставит `tone.level = 0.25` и пишет файл. Ошибка
чтения, разбора или записи → сообщение в stderr и код выхода `1`.

## Вне области

Топология узлов, несколько треков, клипы, автоматизация, MIDI внутри проекта,
бинарный формат, сжатие, миграции версий, автосохранение и undo. Поля версии
нет: до первого релиза формат меняется свободно.
