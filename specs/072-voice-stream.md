# SPEC-072: VoiceStream — голова в RAM, хвост через SPSC-кольца (RFC-002 этап 5)

Статус: ready
Владелец: saytomorrow-st
Для агента: разрешено реализовывать

## Проблема

Банк Osiris Piano целиком (~2.6 GiB PCM) не помещается в бюджет Resident
(SPEC-071). RFC-002 §3–4 предлагает держать в RAM только голову сэмпла, а
хвост подкачивать с диска. Этап 5 — аудио-сторона: структура голоса, которая
читает голову и хвост без аллокаций, блокировок и I/O, и протокол обмена с
потоком-загрузчиком. Сам загрузчик и интеграция в движок — SPEC-073.

## Поведение

- Модуль `crates/audio-core/src/sampler/stream.rs`.
- `SampleHead { data: Arc<[i16]>, total_frames, channels, file_id }` —
  резидентная голова; `has_tail()` = `total_frames > head_frames`.
  `SampleSource::Streaming(SampleHead)` — новый вариант `SampleSource`
  (`source.rs`), конструктор `SampleSource::streaming(head)`,
  `is_streaming()`, `head()`. Без загрузчика такой источник играет только
  голову.
- Константы: `HEAD_FRAMES = 65 536` (по умолчанию), `RING_FRAMES = 32 768`,
  `HEADER_SLOTS = 16`.
- `StreamPool::with_ring_frames(voices, channels, ring_frames)` выделяет всё
  заранее: на каждый слот — кольцо `i16`-сэмплов и кольцо `BlockHeader`
  (`rtrb`), общее кольцо запросов `LoadRequest { voice, generation,
  sample_index, from_frame, frames }` и общие счётчики `StreamStats`.
  Стороны: `VoiceStream`/`StreamRequests` — аудиопоток; `StreamFeeder`/
  `RequestReceiver` — загрузчик.
- Протокол (RFC-002 §4):
  - `start` повышает `generation` слота и запрашивает первый блок хвоста
    (`from_frame = head_frames`);
  - загрузчик кладёт сэмплы, затем заголовок блока с `generation` запроса
    (`push_block`); видимый заголовок ⇒ видимые сэмплы;
  - голос дозапрашивает, когда в кольце осталось ≤ половины; всегда
    `requested − consumed ≤ ring_frames`, так что место в кольце под запрос
    уже зарезервировано;
  - `cancel` (конец ноты, кража, конец сэмпла) повышает `generation`; блоки
    старых поколений вынимаются и отбрасываются (`stale_blocks`).
- **Underrun**: пустое кольцо — не пауза, а fade-out последнего кадра за
  `STEAL_FADE_SAMPLES` (240) сэмплов, `underruns += 1`, голос завершается.
- **Стык голова/хвост**: пока все соседи интерполяции внутри головы, читаются
  те же функции, что для Resident (`read_hermite_i16` и др.) с полной длиной
  сэмпла; дальше четыре соседа Hermite собираются из головы и 4-кадрового
  окна кольца и подаются в то же ядро в том же порядке операций. Результат
  бит-в-бит равен резидентному голосу.
- Реалтайм: `start`, `cancel`, `next_frame` не аллоцируют, не блокируют, не
  делают I/O и не паникуют. `VoiceStream` не держит `Arc`, голова передаётся
  по ссылке — аудиопоток никогда не освобождает память сэмпла. `unsafe` нет.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| Сэмпл короче головы | хвост не запрашивается, играет как Resident |
| Кольцо пусто в момент чтения | fade-out 240 сэмплов, `underruns += 1` |
| Блок старого `generation` | отбрасывается, `stale_blocks += 1` |
| Кольцо запросов переполнено | `dropped_requests += 1`, далее underrun |
| Блок неверной длины | `FeedError::BadLength` у загрузчика |
| Нет места в кольце | `FeedError::Full`, загрузчик повторяет позже |

## Критерии приёмки

- [ ] Потоковое чтение (Linear и Hermite4, разные отношения высоты) бит-в-бит
      совпадает с Resident, включая стык голова/хвост.
- [ ] Underrun даёт монотонный fade-out и тишину после него.
- [ ] `cancel` делает блоки старого поколения устаревшими.
- [ ] no-alloc тест (`tests/sampler_no_alloc.rs`-образец) для `start`,
      `next_frame`, `cancel`, underrun.
- [ ] fmt, clippy `-D warnings`, `cargo test --workspace --all-targets` зелёные;
      golden WAV не изменены.

## Вне области

Поток загрузчика и интеграция в движок (SPEC-073), `SyncStream` и
stress-тесты (этап 7), моно-стриминг, релизы/теги/AppImage.
