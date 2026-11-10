# Аудит: FreePats Drawbar Organ Emulation

- **id**: `freepats-drawbar-organ`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://freepats.zenvoid.org/Organ/electric-organ.html>
- **download.url**: <https://freepats.zenvoid.org/Organ/DrawbarOrganEmulation/DrawbarOrganEmulation-SFZ-20190712.tar.xz>

## Архив

- **format**: `tar.xz`
- **size_bytes**: 6042972
- **sha256**: `e2da18b0a4d13be7020037e18e4a719387433357e7603d0773990e794dcf5d0f`

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
- **Основной `.sfz`**: `DrawbarOrganEmulation-SFZ-20190712/DrawbarOrganEmulation-20190712.sfz`.

## Редиректы

`download.url` отвечает `200` напрямую (без редиректа); `download.redirect_hosts` пуст.
SHA-256 архива гарантирует целостность.
