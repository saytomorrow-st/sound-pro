# Аудит: FreePats Rock Organ Emulation

- **id**: `freepats-rock-organ`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://freepats.zenvoid.org/Organ/electric-organ.html>
- **download.url**: <https://freepats.zenvoid.org/Organ/RockOrganEmulation/RockOrganEmulation-SFZ-20190715.tar.xz>

## Архив

- **format**: `tar.xz`
- **size_bytes**: 12576272
- **sha256**: `07321d9498dc31982c44ea9483919df3f426e3d392a15bb2947b0e3fe247f718`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Published under the terms of Creative Commons CC0 public domain dedication»
- **license_check**: mode `text-hash` — SHA-256 нормализованного текста
  страницы фиксируется и сравнивается:
  `96dcffa82af2b1656aa076ed230f2e9d6d53b0a60cde2e322333febcc6c2d6d3`
  (URL `https://freepats.zenvoid.org/Organ/electric-organ.html` — страница инструмента со строкой
  «Published under the terms of the Creative Commons CC0 1.0 public
  domain dedication»).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `RockOrganEmulation-SFZ-20190715/RockOrganEmulation-20190715.sfz`.

## Редиректы

`download.url` отвечает `200` напрямую (без редиректа); `download.redirect_hosts` пуст.
SHA-256 архива гарантирует целостность.
