# Аудит: Osiris Piano

- **id**: `osiris-piano`
- **Дата проверки**: 2026 (SPEC-067, PR 6)
- **Метод**: реальная загрузка архива и страницы лицензии с апстрима.

## Источник

- **source_url**: <https://github.com/sfzinstruments/Osiris_Piano>
- **download.url**: <https://github.com/sfzinstruments/Osiris_Piano/releases/download/v0.925/Osiris_Piano_0925.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 458671376
- **sha256**: `54ef210b671ea38c4e77291714b5376af28f35803f7c1338737d0f402de02768`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **license_check**: mode `metadata` — получаем SPDX из
  `https://api.github.com/repos/sfzinstruments/Osiris_Piano/license`
  (поле `license.spdx_id`), сравниваем с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение идёт по SPDX, а не по хэшу текста).

## Замечания для интеграции

SFZ использует продвинутые opcode (sustain_cc, amplitude_ccN, offset_ccN,
tune_ccN, note_polyphony, set_cc, `#include`) за пределами подмножества
SPEC-062, а также `sample=../UC/...` (относительный путь с `..`). Загрузка
работает, но звук воспроизводится в «упрощённом» виде; часть артикуляций
не активируется. Это известное ограничение, задокументировано в PR, а не
исправлено молча.
