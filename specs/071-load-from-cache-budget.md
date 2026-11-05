# SPEC-071: Загрузка из PCM-кэша и бюджет памяти (RFC-002 этап 4)

Статус: ready
Владелец: saytomorrow-st
Для агента: разрешено реализовывать

## Проблема

Банк Osiris Piano (1242 FLAC, 48 kHz/2ch/16-bit) в распакованном виде занимает
~2622 MiB. На машине владельца (~7.7 GiB RAM, ~1.3 GiB свободно) текущий
`load_bank_with_hint` декодирует все FLAC в RAM и падает по OOM. RFC-002 этап 4
переводит загрузку на чтение предсозданного PCM-кэша (`.pcm`, `i16`) с бюджетом
памяти по фактически доступной памяти (`MemAvailable`), а не по фиксированному
`DEFAULT_RAM_BUDGET_MIB`.

## Поведение

- `load_bank_from_cache(dir, hint, budget_mib, progress, cancel)` читает
  `cache/manifest.json` и каждый `cache/*.pcm` в `SampleSource::Resident`
  (`Arc<[i16]>`) **без** декодирования FLAC. Регионы резолвятся тем же
  `resolve_unique_samples`, что и при FLAC-загрузке, поэтому `sample_index`
  и порядок сэмплов совпадают.
- **L0** выполняется синхронно при загрузке (существование, размер, заголовок
  `SPCM`, согласованность rate/channels/frames с манифестом). Повреждённые
  записи карантинятся и при наличии источника пересобираются через
  `recover_bank_cache`; запись с неперестраиваемым источником пропускается и
  попадает в `decode_failures`.
- **L1/L2** запускаются в фоновом потоке после успешной загрузки (RFC-002 §6.2):
  L1 всегда, L2 — раз за сессию или при `cache/.dirty`. Результат логируется;
  найденные несоответствия карантинятся и пересобираются фоном.
- `LoadedBank` получает поле `cache: Option<CacheHandle>` (`bank_id`, путь к
  кэшу, `total_bytes`, число записей, `format_version`). `ram_bytes` считает
  только резидентные данные (`Σ 64 + frames×channels×2`).
- **Бюджет** берётся как `effective_budget_mib(DEFAULT_RAM_BUDGET_MIB)` из
  нового `meminfo`-модуля: `min(default, MemAvailable − 256 MiB)`, но не ниже
  512 MiB. При нехватке (`needed > budget`) возвращается
  `BankLoadError::RamBudgetExceeded` — UI показывает предупреждение, а не падает.
- **Банки без кэша** (скачанные старой версией): `preload_bank` строит кэш
  фоном с прогрессом (`build_bank_cache`), затем грузит из кэша. При недоступном
  кэше и неудачной сборке — fallback на существующий FLAC-декодер.
- **Логи**: RSS процесса до/после загрузки (`current_rss_mib`), размер кэша,
  выбранный бюджет, результат проверки целостности.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| `cache/manifest.json` отсутствует | фоновое построение кэша, затем загрузка из него |
| `format_version` не совпал | полная пересборка кэша |
| Запись повреждена (L0) | карантин + пересборка из источника; иначе `decode_failures` |
| `Σ ram_bytes > budget` | `RamBudgetExceeded`, UI-предупреждение |
| `MemAvailable` недоступен (не Linux) | используется `DEFAULT_RAM_BUDGET_MIB` |
| Источник отсутствует | запись пропускается, `decode_failures`, банк грузится |
| `cancel` взведён | загрузка/сборка прерывается на ближайшей границе |

## Критерии приёмки

- [ ] `load_bank_from_cache` возвращает `LoadedBank` с `cache: Some(_)`, без
      вызова FLAC-декодера; `samples[i].as_i16()` совпадает с результатом
      FLAC-загрузки бит-в-бит.
- [ ] Повреждённый/недостающий `.pcm` пересобирается или попадает в
      `decode_failures`.
- [ ] Бюджет по `MemAvailable` ограничивает загрузку; тест `effective_budget_mib_with`.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --all-targets` зелёные.
- [ ] Тесты без аудиоустройства (offline, только файловый I/O).

## Вне области

Стриминг (этапы 5–7), Opus, mmap в аудиопотоке, релизы/теги/AppImage.
