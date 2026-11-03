# SPEC-067: скачивание банков с перепроверкой лицензии

Статус: ready
Владелец: saytomorrow
Для агента: разрешено реализовывать

Файлы: `specs/067-bank-download.md` (новый),
`crates/daw-app/src/bank_download.rs` (новый),
`crates/daw-app/src/bank_library.rs` (расширение),
`crates/daw-app/src/lib.rs`, `crates/daw-app/Cargo.toml`,
`catalog/banks/salamander-grand-v3.json`, `catalog/banks/osiris-piano.json`
(заполнение), `catalog/audit/salamander-grand-v3.md`,
`catalog/audit/osiris-piano.md` (новые), `catalog/README.md`,
`crates/daw-app/tests/bank_download.rs` (новый),
`crates/daw-gui/src/main.rs` (экран принятия, прогресс, отмена, подключение
диалога первого запуска).

Ссылка: `docs/rfc/001-sampler-sfz.md`, раздел 6 «Управление библиотекой
банков», таблица «Фазы и PR» (PR 6). Это PR 6 вертикального среза сэмплера:
HTTP-скачивание с перепроверкой лицензии перед **каждой** загрузкой, проверка
SHA-256 архива, безопасная распаковка, атомарная установка и запись в реестр.
Переиспользует каталог-манифест (`bank_library.rs`, SPEC-066), загрузку банка
(`sample_bank.rs`, SPEC-064) и выбор инструмента (`instrument.rs`,
`TrackInstrument`, SPEC-065) как есть — без дублирования.

## Проблема

SPEC-066 добавил каталог-манифест, реестр установленных банков, окна
«Библиотека звуков»/«Авторы» и диалог первого запуска, но кнопка «Скачать»
заблокирована (`sha256: null`, `verified: false`), а «Скачать звуки сейчас» в
диалоге первого запуска лишь открывает библиотеку. Сейчас единственный способ
получить банк — локальная папка. Нужно (1) реальное скачивание с оригинальных
URL из манифеста с закреплённым SHA-256 и лимитом размера, (2) перепроверка
лицензии перед **каждой** загрузкой (fetch `license_check`, сравнить хэш и
SPDX), (3) безопасная распаковка с защитой от zip-slip/zip-бомб/симлинков,
(4) атомарная установка в каталог данных и запись в реестр с атрибуцией,
(5) экран принятия (автор, лицензия, атрибуция, размер) до скачивания,
(6) скачивание вне UI-потока с прогрессом и отменой.

## Решения владельца (обязательные)

- **Мы ничего не хостим.** Скачивание идёт только с upstream-URL из
  манифеста каталога; никаких зеркал и зашитых локальных копий.
- Лицензия перепроверяется перед **каждой** загрузкой: fetch источника
  лицензии (`license_check.url` / upstream-метаданные), сравнить хэш текста
  (`sha256`) с аудированным; при несовпадении, недоступности источника или
  отсутствии хэша — **блокировать** загрузку и показать понятное
  предупреждение. Приложение **не** предлагает «качать всё равно».
- Целостность архива: закреплённый SHA-256 из манифеста проверяется до
  распаковки; несовпадение — удалить частичные файлы и ошибка.
- Перед скачиванием — экран принятия: имя лицензии, автор, текст атрибуции,
  размер. Атрибуция сохраняется в реестре и показывается в окне «Авторы».
- Диалог первого запуска «Скачать сейчас / Позже» из SPEC-066 теперь ведёт
  к реальному скачиванию.

## Поведение

### 1. Расширение манифеста (`bank_library.rs`)

`CatalogEntry` получает поля (обратная совместимость через `serde default`):

```json
{
  "schema": 1,
  "id": "osiris-piano",
  "name": "Osiris Piano",
  "family": "piano",
  "version": "0.925",
  "author": "Versilian Studios and Karoryfer Samples",
  "license": { "spdx": "CC0-1.0", "url": "https://creativecommons.org/publicdomain/zero/1.0/",
               "attribution_required": false },
  "attribution": "Osiris Piano by Versilian Studios and Karoryfer Samples, CC0 1.0",
  "source_url": "https://github.com/sfzinstruments/Osiris_Piano",
  "license_check": {
    "url": "https://api.github.com/repos/sfzinstruments/Osiris_Piano/license",
    "sha256": null,
    "mode": "metadata"
  },
  "sfz": "Osiris Piano/Programs/01-natural.sfz",
  "format": "zip",
  "download": {
    "url": "https://github.com/sfzinstruments/Osiris_Piano/releases/download/v0.925/Osiris_Piano_0925.zip",
    "sha256": "<64 hex>",
    "size_bytes": 458671376
  },
  "verified": true
}
```

Новые типы и поля:

- `LicenseCheck { url, sha256, mode }`, где `mode ∈ {"text-hash",
  "metadata"}`:
  - `text-hash` — `url` отдаёт текст лицензии (или страницу), который
    нормализуется (UTF-8, обрезка пробелов, `\r\n`→`\n`) и сравнивается по
    SHA-256. `sha256` обязателен.
  - `metadata` — `url` отдаёт JSON-метаданные (например, GitHub API
    `license.spdx_id`); извлекается и сравнивается только SPDX-id с
    `license.spdx`. `sha256` — `null` (хэш всего тела ответа GitHub API
    хрупок: GitHub может добавлять поля).
- `Download { url, sha256, size_bytes }` — один архив для простого банка.
- `sfz: Option<String>` — путь к главному `.sfz` внутри архива
  (`/`-разделители). `None`/пусто — «первый `.sfz` в корне» (правило
  `find_sfz` из SPEC-064).
- `format: String` — `"zip"` или `"tar.bz2"` (архивы Salamander —
  `.tar.bz2`, см. §5).

Старые поля `sha256`/`size_bytes` (скалярные) сохраняются для обратной
совместимости тестов, но загрузчик читает только `download`. Запись без
`download` (или с `verified: false`, или с пустым `download.sha256`) —
**не скачиваема** (кнопка «Скачать» disabled), как и было в SPEC-066.

Валидация добавляет: для скачиваемой записи (`download` присутствует)
обязательны непустые `download.url`, `download.sha256` (64 hex), непустой
`download.size_bytes`, непустой `license_check.url`, допустимый
`license_check.mode`; для `mode: "text-hash"` обязателен непустой
`license_check.sha256`, для `mode: "metadata"` — `sha256` может быть `null`;
`format ∈ {"zip", "tar.bz2"}`.
Нарушение — новые варианты `CatalogError` (`BadDownload`, `BadLicenseCheck`,
`BadFormat`).

### 2. HTTP-клиент (rustls, без OpenSSL)

Зависимость: `ureq` с `rustls` (вместо `reqwest` — меньший бинарь, синхронный
API достаточен: загрузка идёт в отдельном потоке). `ureq` тянет `rustls`
(→ `ring`, чистый Rust; собирается C-компилятором только на машине сборки, у
пользователя ничего не требуется) и `rustls-native-certs`/`webpki-roots` для
корневых сертификатов. `url` — только для `Url::join` (относительные
редиректы по RFC 3986) и сравнения host/port; `tempfile` — только
dev-dependency для изолированных временных каталогов в тестах. Выбор и цена
по размеру — в §7 «Риски».

Поведение клиента (тип `HttpOptions`):

- **Таймауты**: connect 10 с, общий read 30 с (разрешает медленное
  скачивание большого архива), для `license_check` — read 10 с.
- **Redirects**: каждый `download` в каталоге несёт `redirect_hosts`
  allow-list (только HTTPS; точный хост или `.suffix`). Редирект на хост из
  списка разрешён, на невключённый хост — `RedirectToOtherHost`, а
  понижение HTTPS→HTTP — `InsecureUrl` (целостность держит SHA-256). Относительные
  `Location` резолвятся через `Url::join` (RFC 3986); сравнение хостов —
  регистронезависимо с нормализацией портов по умолчанию. Максимум 5
  редиректов; 3xx без `Location` — ошибка `RedirectMissingLocation`, а не
  `TooManyRedirects`.
- **TLS**: только HTTPS для данных (`download.url`), `rustls` с проверкой
  сертификата. `http://` для данных запрещён (ошибка `InsecureUrl`).
- **Размер**: `Content-Length` сверяется с `download.size_bytes` (лимит);
  фактически прочитанные байты не могут превысить `size_bytes` (стриминг в
  файл с счётчиком; превышение → `SizeExceeded`, частичный файл удаляется).
- **Стриминг**: тело читается блоками (64 КиБ) в `BufWriter` временного
  файла, счётчик байт публикуется через `Arc<AtomicU64>` для прогресса.

### 3. Перепроверка лицензии (`license_check`)

Порядок перед скачиванием данных (всегда):

1. `GET license_check.url` с лимитом 1 МиБ и таймаутом 10 с.
2. Нормализовать тело: UTF-8 (невалидный → `BadLicenseBody`), обрезка
   пробелов по краям, `\r\n`→`\n`.
3. Для `mode: "text-hash"` — вычислить SHA-256 нормализованного тела и
   сравнить с `license_check.sha256`. Несовпадение → `LicenseHashMismatch`
   (показать оба хэша).
4. Для `mode: "metadata"` — распарсить JSON и сравнить поле
   `license.spdx_id` (или `spdx`) с `license.spdx`. Несовпадение →
   `LicenseSpdxMismatch`.
5. Любая ошибка (нет сети/таймаут/5xx/404/недоступный хост/редирект на
   другой хост) → блокировка с соответствующим сообщением.

Только после успешной проверки показывается экран принятия и начинается
скачивание.

### 4. Скачивание и распаковка (`bank_download.rs`)

Функция `download_bank(entry, progress, cancel) -> Result<InstalledBank,
DownloadError>` (выполняется в отдельном потоке):

1. Перепроверить лицензию (§3).
2. Скачать архив во временный файл в каталоге данных
   (`banks/.tmp/<id>.partial`), стримингом, с прогрессом и проверкой отмены
   (флаг `Arc<AtomicBool>` проверяется между блоками; отмена → `Cancelled`,
   частичный файл удаляется).
3. Проверить фактический размер против `download.size_bytes` и
   SHA-256 против `download.sha256` (`sha2`). Несовпадение → удалить
   частичный файл, `ArchiveHashMismatch` / `ArchiveSizeMismatch`.
4. Безопасно распаковать (см. §5) во временный каталог
   (`banks/.tmp/<id>.unpack`), затем **атомарно** переименовать в
   `banks/<id>/` (тот же приём, что в `pcm_cache`: запись во временную
   директорию, `rename`). Существующий банк заменяется только после успешной
   распаковки.
5. Скопировать `LICENSE.txt` банка (текст из `license.url`/`license_check`
   при `text-hash`, либо строка `license.spdx` + `license.url`), записать
   запись в реестр (`install`-подобный путь, но с подтверждённой
   лицензией).

Ошибки типизированы `DownloadError` (см. §6), текст сообщений — на русском
(как в остальном GUI) с коротким английским пояснением.

### 5. Безопасная распаковка

**zip** (`zip`-крейт, чистый Rust):

- Отклонить запись с абсолютным путём (`/…`, `C:\…`, UNC), с `..`-компонентой
  или содержащую `\` → ошибка `UnsafeEntry` (zip-slip).
- Отклонить симлинк/хардлинк-записи (`zip`-крейт: `unix_mode` с S_IFLNK) →
  `UnsafeEntry`.
- Отклонить запись, чей суммарный распакованный размер превышает лимит
  (по умолчанию `unpack_limit = size_bytes × 8`, максимум 4 ГиБ) → защита от
  zip-бомбы.
- Каждая запись распаковывается потоком в файл внутри временной директории;
  путь строится только из очищенного имени.

**tar.bz2** (Salamander, `tar` + `bzip2`-крейты, чистый Rust): те же правила
(абсолютные пути/`..`/симлинки/лимит), распаковка через `tar::Archive` над
`Bzip2Decoder`.

Записи с `..` внутри банка (Osiris использует `sample=../UC/A/…` и
`#include` — это **не** путь в архиве, а ссылка внутри SFZ) — легальны в
**содержимом** архива; запрет касается только **путей записей архива**.

### 6. Термины ошибок (`DownloadError`)

| Вариант | Условие |
|---|---|
| `LicenseFetchFailed { url, reason }` | не удалось скачать лицензию (сеть/таймаут/5xx) |
| `LicenseHashMismatch { expected, actual }` | хэш лицензии не совпал |
| `LicenseSpdxMismatch { expected, actual }` | SPDX не совпал (`metadata`) |
| `BadLicenseBody` | невалидный UTF-8/JSON |
| `RedirectToOtherHost { from, to }` | редирект на другой хост |
| `InsecureUrl { url }` | `http://` для данных |
| `SizeExceeded { expected, actual }` | скачано больше лимита |
| `ArchiveSizeMismatch` / `ArchiveHashMismatch` | размер/хэш архива не совпал |
| `UnsafeEntry { name }` | zip-slip/симлинк |
| `UnpackLimitExceeded { limit }` | распаковано больше лимита |
| `Cancelled` | отмена пользователем |
| `Io { path, source }` | прочие ошибки I/O |

### 7. GUI

- **Экран принятия** (модальное окно) перед скачиванием: имя банка, автор,
  лицензия (SPDX + ссылка), текст атрибуции, размер загрузки и распакованный
  размер. Кнопки «Принять и скачать» / «Отмена».
- **Прогресс и отмена**: строка прогресса (байты / размер, проценты),
  кнопка «Отмена». Скачивание — в фоновом потоке; результат в UI через
  `std::sync::mpsc` (UI опрашивает раз в кадр, как фоновый импорт).
- Кнопка «Скачать» в библиотеке теперь активна для записей с заполненным
  `download`/`verified`; ведёт к экрану принятия → скачиванию.
- Диалог первого запуска «Скачать звуки сейчас» ведёт к экрану принятия для
  рекомендованного банка (Salamander), затем к скачиванию.
- Ошибки — читабельно на русском (английский как пояснение), в строке статуса
  библиотеки и/или во всплывающем окне.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| лицензия недоступна / таймаут / 5xx | блок, «Не удалось проверить лицензию», «Повторить» |
| хэш лицензии не совпал | блок, показать оба хэша |
| SPDX не совпал (`metadata`) | блок, «Лицензия у автора изменилась» |
| редирект на другой хост | блок, «Источник изменился» |
| `http://` для данных | блок, `InsecureUrl` |
| хэш архива не совпал | удалить частичный файл, ошибка |
| размер архива превысил лимит | удалить частичный файл, `SizeExceeded` |
| zip-slip (абсолютный путь/`..`/симлинк) | `UnsafeEntry`, ничего не записано |
| zip-бомба (суммарно > лимит) | `UnpackLimitExceeded`, ничего не записано |
| отмена во время скачивания | `Cancelled`, частичный файл удалён |
| запись с `download` отсутствует/`verified: false` | кнопка disabled |
| повторное скачивание установленного банка | заменяет банк после успешной распаковки |

## Тесты (локальный HTTP-сервер, без реальной сети)

`crates/daw-app/tests/bank_download.rs` поднимает `std::net::TcpListener` на
`127.0.0.1:0` (порт 0 = произвольный) и обслуживает запросы в тестовом
потоке. Все случаи:

1. **success** — валидные `license_check` + архив; банк установлен, реестр
   обновлён, атрибуция сохранена.
2. **hash mismatch** — архив с неверным SHA-256 → `ArchiveHashMismatch`,
   частичный файл удалён.
3. **license mismatch** — `license_check` с неверным SHA-256 →
   `LicenseHashMismatch`, данные не скачивались.
4. **truncated download** — сервер обрывает соединение до `Content-Length` →
   ошибка, частичный файл удалён.
5. **redirect to other host** — `Location: http://evil.example/…` →
   `RedirectToOtherHost`.
6. **zip-slip archive** — архив с записью `../evil.txt` или абсолютным путём →
   `UnsafeEntry`.
7. **oversize archive** — `Content-Length`/фактический размер > лимита →
   `SizeExceeded`.
8. **cancel** — флаг отмены взводится после начала → `Cancelled`.

Тесты детерминированы (нет sleep/wall-clock; ожидание через чтение из
сокета). Используются только маленькие фикстурные архивы, создаваемые в тесте
через `zip`/`tar`-крейты.

## Критерии приёмки

- [ ] `cargo fmt --all --check` зелёный
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` зелёный
- [ ] `cargo test --workspace --all-targets` зелёный (offline; фича
      `realtime` не собирается локально, проверяется CI-джобой
      «realtime backend build», которая должна пройти с TLS-зависимостью на
      ubuntu и windows)
- [ ] Манифест: новые поля валидируются; нескачиваемая запись остаётся
      disabled; `sha256`/размер/`license_check` заполнены **только** реально
      проверенными значениями (см. ниже).
- [ ] `bank_download.rs`: перепроверка лицензии перед каждой загрузкой,
      SHA-256 архива, безопасная распаковка, атомарная установка, реестр.
- [ ] GUI: экран принятия (автор/лицензия/атрибуция/размер), прогресс,
      отмена, читабельные русские ошибки; первый запуск ведёт к скачиванию.
- [ ] Тесты с локальным HTTP-сервером (8 случаев выше) зелёные.
- [ ] Goldens не меняются; не ослабляются существующие тесты.

## Проверенные upstream-значения (заполнять только эти)

Оба значения хэшей и размеров получены реальным скачиванием архива и
вычислением SHA-256; лицензии — fetch'ем upstream. **Не выдумывать**; если
что-то изменить/недоступно — оставить `sha256: null`/`verified: false` и
сообщить в PR.

### Osiris Piano (zip, CC0-1.0)

- `download.url`: `https://github.com/sfzinstruments/Osiris_Piano/releases/download/v0.925/Osiris_Piano_0925.zip`
- `download.sha256`: `54ef210b671ea38c4e77291714b5376af28f35803f7c1338737d0f402de02768`
- `download.size_bytes`: `458671376`
- `license_check.url`: `https://api.github.com/repos/sfzinstruments/Osiris_Piano/license`
- `license_check.mode`: `metadata` (SPDX из `license.spdx_id` = `CC0-1.0`,
  `sha256` = `null`)
- `sfz`: `Osiris Piano/Programs/01-natural.sfz`
- Архив: 1361 запись, верхняя папка `Osiris Piano`, 74 SFZ + 1242 FLAC, без
  симлинков/абсолютных путей/`..` в путях записей.

**Предупреждение о ремаппинге (для PR, не тихое исправление).** Osiris
использует продвинутые опкоды (`sustain_cc`, `ampeg_releasecc64`,
`amplitude_ccN`, `offset_ccN`, `tune_ccN`, `note_polyphony`, `set_cc*`,
`#include`), которых нет в поддерживаемом подмножестве SPEC-062; они будут
проигнорированы с предупреждением, и много-микрофонные программы загрузятся в
упрощённом виде (один слой микрофона). Кроме того, `sample=`-ссылки вида
`sample=../UC/A/Piano_UC_MicA_A-1_vl1.flac` используют `..` относительно
`Programs/modules/mappings/`, что `confine_path` разрешает **в пределах корня
банка** (обратный слэш нормализуется в `/`, `..` не может выйти выше корня —
иначе `None` и запись о decode-failure). Это документируется в PR как
известное ограничение, а не исправляется молча.

### Salamander Grand Piano V3 (tar.bz2, CC-BY-3.0)

- `download.url`: `https://archive.org/download/SalamanderGrandPianoV3/SalamanderGrandPianoV3_44.1khz16bit.tar.bz2`
- `download.sha256`: `fe595c7722b70860e6377f82948d1c0cfcf27ecebf500dc79534408a78a62892`
- `download.size_bytes`: `488713261`
- `license_check.url`: `https://creativecommons.org/licenses/by/3.0/legalcode.txt`
- `license_check.mode`: `text-hash` (хэш нормализованного текста CC-BY 3.0
  legalcode: `11dbf7126ae9794e4a7dc54334f3c82a67efcf22df888d396dc2ed707b2a3f9b`)
- `sfz`: `SalamanderGrandPianoV3_44.1khz16bit/SalamanderGrandPianoV3.sfz`
- Формат: `.tar.bz2` (WAV 16-бит, **не** FLAC/zip как в черновике RFC),
  верхняя папка `SalamanderGrandPianoV3_44.1khz16bit/`, внутри
  `44.1khz16bit/*.wav` (641 WAV) + `SalamanderGrandPianoV3.sfz` +
  `SalamanderGrandPianoV3Retuned.sfz` + `README`.

**Предупреждение о путях (для PR).** SFZ использует `sample=44.1khz16bit\A0v1.wav`
(обратный слэш `\` как разделитель) — при разрешении сэмплов путь
нормализуется (`\`→`/`) в `confine_path` (покрыто тестом
`confine_path_normalises_backslashes`), так что на Linux/Windows файлы
находятся.

**Замечание о лицензии Salamander.** `archive.org` отдаёт только
md5/sha1/crc32 (не sha256); SHA-256 вычислен вручную из скачанного архива.
Лицензия CC-BY-3.0 подтверждена `licenseurl` в метаданных archive.org.
`license_check` для Salamander — `text-hash` по тексту CC-BY 3.0 legalcode с
creativecommons.org (стабильный канонический текст); если его хэш не удаётся
зафиксировать стабильно, оставить `verified: false` и сообщить в PR.

## Нельзя

- Менять `crates/audio-core/**` (движок/сэмплер), goldens, `vendor/winit`,
  `SOUND_PRO_PATCH.md`, `x11_cursor.rs`, `tests/drop.rs`,
  `docs/PROJECT_STATE.md`, `docs/ROADMAP.md`.
- Использовать OpenSSL или системные TLS-библиотеки (rustls только).
- Тихо редактировать SFZ-файлы или ремаппить пути сэмплов без упоминания в PR.
- Выдумывать SHA-256/размеры/license_check: только реально проверенные
  значения выше.
- Использовать `unsafe`, аллокации/блокировки/I/O в аудиоколбэке.
- Ослаблять существующие тесты или добавлять `#[ignore]`.

## Вне области

Потоковая подгрузка (PR 7), Hermite-интерполяция, живая нода `SamplerNode`
(нужен MIDI-вход), профили `lite`, `catalog/audit` для прочих банков
(virtuosity_drums, VSCO-2-CE) — будущие PR.
