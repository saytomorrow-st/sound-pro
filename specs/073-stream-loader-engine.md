# SPEC-073: Поток загрузчика и Streaming в движке и банке (RFC-002 этап 6)

Статус: ready
Владелец: saytomorrow-st
Для агента: разрешено реализовывать

## Проблема

Этап 5 (SPEC-072) дал `VoiceStream`, но его кольца никто не наполняет, а
`SamplerEngine` и загрузка банка про Streaming не знают. Osiris Piano (~2.6 GiB
PCM) превышает бюджет Resident и не загружается. Цель — банк сверх бюджета
грузится в режиме Streaming (головы в RAM, хвосты из `.pcm` кэша), чтобы
Osiris укладывался в ~300–450 MiB RSS.

## Поведение

### Движок (`audio-core`, `sampler/engine.rs`)

- `SamplerEngine::enable_streaming(ring_frames) -> StreamLoaderEnds`
  (control thread): сбрасывает движок и выделяет `2 × voices` стерео-слотов
  (голос + хвост затухания при краже). Возвращает стороны загрузчика
  `{ feeders, receiver, stats }`. Повторный вызов отменяет все потоки,
  старые стороны становятся неактуальными.
- `note_on` для `SampleSource::Streaming` берёт свободный слот и стартует
  поток, только если регион может стримиться: стерео, `offset == 0`, `end`
  не внутри сэмпла, и `loop_mode` = `no_loop`/`one_shot` либо петля
  (`loop_continuous`/`loop_sustain`) с `loop_start < head_frames / 2` и
  `loop_end` за пределами головы (`0` = конец сэмпла, как в Osiris). Иначе
  (нет свободного слота, стриминг не включён, петля целиком в голове или
  начинается вне её) играет только голова / резидентная петля.
- Петля: при переходе `loop_end → loop_start` движок вызывает
  `VoiceStream::rewind(pos)` — `generation` повышается (блоки прошлого
  прохода устаревают, остаток текущего блока отбрасывается), поток
  перезапрашивает хвост от стыка; голова покрывает начало петли, пока
  загрузчик дочитывает. Вывод бит-в-бит равен Resident на нескольких проходах.
- Слот освобождается при конце сэмпла, underrun, note-off после затухания,
  краже, `reset` — каждый раз через `cancel` (повышение `generation`).
- `is_streaming()`, `stream_stats()`, `active_streams()`.
- Аудиопоток: без аллокаций, блокировок и I/O; слоты — заранее выделенный
  массив флагов.

### Загрузчик (`daw-app`, `stream_loader.rs`)

- `StreamLoader::new(ends, files)` — `files[sample_index]` = путь `.pcm`.
- `pump()` — синхронно обслуживает все запросы, для которых есть место в
  кольце; остальные откладывает. Устаревшие (`!is_current(generation)`)
  пропускает до и после чтения. Читает `PCM_HEADER_LEN + from_frame × 4`
  через позиционное чтение (`read_exact_at`/`seek_read`), держит до 256
  открытых файлов.
- `spawn()` — фоновый поток `sp-stream-loader`, крутит `pump`, при простое
  спит 1 мс; `LoaderThread` останавливает и join-ит поток в `Drop`.
- `LoaderStats { blocks, skipped_stale, read_errors }`. Ошибка чтения
  логируется, запрос снимается — голос уходит в fade-out (underrun).

### Банк (`daw-app`, `sample_bank.rs`)

- `load_bank_from_cache_streaming(dir, hint, budget)`: стерео-записи длиннее
  `STREAM_HEAD_FRAMES = 32 768` кадров загружаются как `SampleSource::Streaming`
  (голова читается `read_pcm_head`), остальные — Resident. `LoadedBank`
  получает `stream_files: Vec<Option<PathBuf>>` и `is_streaming()`;
  `ram_bytes` считает только резидентное. Если даже головы не влезают в
  бюджет — `RamBudgetExceeded`.
- `load_bank_auto` / `load_bank_auto_with_budget`: при `RamBudgetExceeded`
  в Resident-режиме автоматически повторяют загрузку в Streaming.
- `SamplerInstrument` для Streaming-банка включает стриминг в движке и
  вызывает `pump` перед каждым блоком 1024 кадров — offline-рендер
  детерминирован и бит-в-бит равен Resident.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| Банк в бюджете | Resident, как раньше |
| Банк сверх бюджета, головы влезают | Streaming |
| Головы не влезают | `RamBudgetExceeded` |
| `.pcm` пропал/не читается | `read_errors += 1`, fade-out голоса |
| Note-off / смена банка во время чтения | `generation` устарел, блок пропущен |
| Петля с `loop_start` в голове | стримится, `rewind` на каждом проходе |
| Петля начинается вне головы | играет голова, без стриминга |
| Кончились слоты | играет голова |

## Критерии приёмки

- [ ] Streaming-рендер через `StreamLoader::pump` бит-в-бит равен Resident,
      `underruns == 0`, `blocks > 0`.
- [ ] Отсутствующий файл: `read_errors > 0`, один underrun, тишина после
      затухания, слоты освобождены.
- [ ] Петля (вся длина и `loop_start` внутри головы) стримится бит-в-бит как
      Resident на нескольких проходах, `underruns == 0`.
- [ ] `load_bank_auto_with_budget` при малом бюджете возвращает Streaming-банк,
      в бюджете — Resident.
- [ ] `SamplerInstrument` из Streaming-банка рендерит бит-в-бит как из Resident.
- [ ] `LoaderThread` корректно завершается при `Drop`.
- [ ] no-alloc тест рендера движка со стримингом (кража, note-off, underrun,
      `reset`, перемотка петли).
- [ ] fmt, clippy `-D warnings`, `cargo test --workspace --all-targets` зелёные;
      golden WAV не изменены.

## Вне области

`SyncStream`, stress-тесты (этап 7), подключение `LoaderThread` к
realtime-ноде GUI, моно-стриминг, релизы/теги/AppImage.
