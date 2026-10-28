# MIDI: поддержка SMF (SPEC-004)

Парсер Standard MIDI File в `audio-core::midi`. Только чтение из байт, без
файлового ввода-вывода, без платформенного кода, без `unsafe`. На мусорных
входах возвращает `MidiError`, а не паникует.

## Что поддержано

| Возможность | Поддержка |
|---|---|
| Формат файла | 0 и 1 |
| Заголовок `MThd` | да (пропускает лишние байты заголовка по длине) |
| Треки `MTrk` | да; неизвестные чанки пропускаются по длине |
| Деление (division) | PPQN; SMPTE → `UnsupportedDivision` |
| `NoteOn` / `NoteOff` | да (`velocity == 0` = `NoteOff`) |
| `Tempo` meta (0x51) | да, микросекунды на четверть |
| Running status | да |
| `EndOfTrack` (0x2F) | да |
| Прочие meta/SysEx | пропускаются по длине |

## Что не поддержано (вне области SPEC-004)

Запись MIDI, MIDI 2.0, полноценная обработка SysEx, плагины, синтез звука из
нот.

## API

- `parse_smf(bytes: &[u8]) -> Result<MidiFile, MidiError>` — единственная точка
  входа.
- `MidiFile { format, ppqn, track_count, events }`, `events` отсортированы по
  `(tick, track, order_in_track)` — детерминированно.
- `MidiEvent { track, tick, order_in_track, message }`.
- `MidiMessage::{NoteOn, NoteOff, Tempo}`.
- `MidiFile::notes()` — только `NoteOn` в порядке сортировки.

## Граничные случаи

| Вход | Результат |
|---|---|
| пусто | `MidiError::Empty` |
| обрезанный трек | `MidiError::Truncated { at }` |
| `NoteOn` без `NoteOff` до `EndOfTrack` | нота отпускается `NoteOff` на тике EOT |
| битый заголовок / сверхбольшой номер ноты | `MidiError::Malformed { at }` |
| 10 МБ случайных байт | `Err`, без паники, < 1 с |
