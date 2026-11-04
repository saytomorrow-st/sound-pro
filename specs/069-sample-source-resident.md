# SPEC-069: `SampleSource::Resident` — обёртка источника сэмпла без изменения поведения

Статус: ready
Владелец: saytomorrow
Для агента: разрешено реализовывать (этап 0 RFC-002)

Файлы: `specs/069-sample-source-resident.md` (новый),
`crates/audio-core/src/sampler/source.rs` (новый),
`crates/audio-core/src/sampler/mod.rs`,
`crates/audio-core/src/sampler/{engine,voice}.rs`,
`crates/audio-core/src/lib.rs`,
`crates/daw-app/src/sample_bank.rs`,
`crates/daw-app/src/instrument.rs`,
`crates/audio-core/tests/sampler.rs` (при необходимости).

Ссылка: `docs/rfc/002-sample-streaming.md`, §3.1 («Источник сэмпла») и §10
(«План по этапам», этап 0); `docs/rfc/001-sampler-sfz.md`, §3.3.
Это этап 0 вертикального среза RFC-002: ввести перечисление `SampleSource` с
единственным вариантом `Resident(Arc<[i16]>)` и заменить им текущий
`Vec<Arc<[i16]>>` в движке и загрузчике, **не меняя поведение**. Никакого
стриминга, дискового I/O, колец или фоновых потоков в этом этапе нет.

## Проблема

Сейчас движок (`SamplerEngine`) и загрузчик (`LoadedBank`) хранят сэмплы как
`Vec<Arc<[i16]>>` — «весь сэмпл целиком в памяти». RFC-002 вводит источник
сэмпла, который в будущем сможет быть либо резидентным (`Resident`), либо
стриминговым (`Streamed(SampleHead)`), но сам переход к стримингу — этапы 5–7
(другой исполнитель). Чтобы не переписывать `SamplerEngine` дважды, сначала
вводится перечисление `SampleSource`, которое на этом этапе имеет ровно один
вариант и ведёт себя побитово идентично старому `Arc<[i16]>`.

## Поведение

### 1. Тип `SampleSource` (audio-core, новый модуль `sampler::source`)

```rust
use std::sync::Arc;

/// Источник декодированного `i16`-сэмпла.
pub enum SampleSource {
    /// Весь сэмпл целиком в памяти (текущий режим).
    Resident(Arc<[i16]>),
}

impl SampleSource {
    pub fn resident(data: Arc<[i16]>) -> Self;   // Self::Resident(data)
    pub fn as_i16(&self) -> &[i16];              // внутренние данные сэмпла
    pub fn len(&self) -> usize;                  // data.len()
    pub fn is_empty(&self) -> bool;              // data.is_empty()
}
```

`as_i16` возвращает лежащий в основе `&[i16]`: для `Resident(Arc<[i16]>)` это
`&**arc`. Метод не аллоцирует, не блокирует и не делает I/O — его можно
вызывать на горячем пути.

### 2. Замена `Vec<Arc<[i16]>>` в движке

- `SamplerEngine.samples: Vec<Arc<[i16]>>` → `Vec<SampleSource>`.
- `SamplerEngine::new` / `new_with` принимают `Vec<SampleSource>`; сигнатура
  меняется, но семантика та же: `RegionTable::build(&regions, samples.len())`
  использует длину вектора источников.
- `note_on` и `render_voice` читают данные через `sample.as_i16()`, а длину —
  через `sample.len()` (вместо прежнего `sample.len()` по `Arc`, который
  делегировал в `[i16]`). Результат побитово тот же.
- `render_voice(samples: &[Arc<[i16]>])` → `&[SampleSource]`.

### 3. Замена в загрузчике (daw-app)

- `LoadedBank.samples: Vec<Arc<[i16]>>` → `Vec<SampleSource>`, где каждый
  сэмпл обёрнут в `SampleSource::resident(Arc::from(data))`.
- `SamplerInstrument` (daw-app) хранит/пробрасывает `Vec<SampleSource>` в
  `SamplerEngine::new_with`; `SamplerInstrument::samples()` возвращает
  `&[SampleSource]` (или оставляется доступ к данным через `as_i16`).

### 4. Инварианты этапа 0

- Golden WAV (`crates/audio-core/tests/golden/*.wav`) не меняются: DSP-путь
  читает те же `i16`-данные, что и раньше, поэтому выход побитово идентичен.
- Никаких новых зависимостей, потоков, колец, `mmap`, `unsafe`.
- Никакого изменения звука или бюджета RAM; `DEFAULT_RAM_BUDGET_MIB`,
  `RamBudgetExceeded`, порядок декодирования — без изменений.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| пустой сэмпл (`Arc<[i16]>` длины 0) | `SampleSource::resident(Arc::new([]))`; `is_empty() == true`, `as_i16().len() == 0` |
| `as_i16` на `Resident` | возвращает `&[i16]` без аллокации и паники |
| вызов `len`/`is_empty` на горячем пути | без аллокации/блокировки/I/O |
| стерео-сэмпл | `as_i16()` — интерливированный `[L,R,…]`; длина кратна 2 |

## Критерии приёмки

- [ ] `cargo fmt --all --check` зелёный
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` зелёный
- [ ] `cargo test --workspace --all-targets` зелёный (offline-путь)
- [ ] Юнит-тесты `SampleSource` (resident/len/is_empty/as_i16)
- [ ] Существующие golden и тесты сэмплера проходят без изменений
- [ ] `SamplerInstrument`/`load_bank` собираются и рендерят как раньше

## Нельзя

- Добавлять вариант `Streamed`, кольца, loader-поток, `mmap`, фоновые потоки
  (это этапы 5–7).
- Менять golden WAV или ослаблять тесты.
- Добавлять зависимости (`xxhash-rust`, `blake3` — этап 1, только в `daw-app`).
- Использовать `unsafe` в `audio-core` (стоит `#![forbid(unsafe_code)]`).

## Вне области

`.pcm`-формат и манифест (этап 1), кэш-генерация в `download_bank` (этап 2),
integrity L0–L3 и автоперестроение (этап 3), `load_bank_from_cache` и бюджет
(этап 4), стриминг и Opus (этапы 5–7).
