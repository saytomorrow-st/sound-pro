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

## Планировщик: тик → сэмпл (SPEC-007)

`ScheduleSpec::from_midi(&file, sample_rate, start_tempo, tail_samples)`
переводит события из тиков в абсолютные сэмплы в контрольном потоке:

```text
bpm(tick)     = 60_000_000 / micros_per_quarter   (до первого Tempo — start_tempo)
seconds(tick) = seconds(prev) + (tick - prev) / (ppqn * bpm(tick) / 60)
sample        = round(seconds(tick) * sample_rate)
```

- копятся именно **секунды** в `f64`, сэмпл округляется один раз: ошибка
  округления не накапливается и смена темпа не сдвигает уже посчитанное;
- `Tempo` действует с своего тика вперёд; `sample` самого `Tempo`-события
  считается ещё по старому темпу;
- `length_samples` = сэмпл последнего события + `tail_samples` (хвост
  отпускания);
- ошибки: `InvalidPpqn`, `InvalidTempo { tick }` (`micros_per_quarter == 0`),
  `InvalidSampleRate` (`sample_rate == 0`), `Empty` (нет ни одного `NoteOn`).

Пример: 120 bpm, 480 ppqn, 48 кГц → четверть (тик 480) = 0.5 с = 24 000
сэмплов. После `Tempo` 60 bpm следующая четверть занимает вдвое больше.

Исполнение в аудиопотоке — через `Schedule::next_event` с курсором; контракт
реального времени — в [REALTIME.md](REALTIME.md), раздел «Опрос расписания».

## Граничные случаи

| Вход | Результат |
|---|---|
| пусто | `MidiError::Empty` |
| обрезанный трек | `MidiError::Truncated { at }` |
| `NoteOn` без `NoteOff` до `EndOfTrack` | нота отпускается `NoteOff` на тике EOT |
| битый заголовок / сверхбольшой номер ноты | `MidiError::Malformed { at }` |
| 10 МБ случайных байт | `Err`, без паники, < 1 с |
