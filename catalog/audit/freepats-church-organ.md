# Аудит: FreePats Church Organ Emulation

- **id**: `freepats-church-organ`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://freepats.zenvoid.org/Organ/pipe-organ.html>
- **download.url**: <https://freepats.zenvoid.org/Organ/ChurchOrganEmulation/ChurchOrganEmulation-SFZ-20190924.tar.xz>

## Архив

- **format**: `tar.xz`
- **size_bytes**: 13033856
- **sha256**: `1f0b70a2c2af14cdf051ef2d256b3bb652e7d64c0fdc8bed06642ad2af916db9`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Published under the terms of Creative Commons CC0 public domain dedication»
- **license_check**: mode `text-hash` — SHA-256 нормализованного текста
  страницы фиксируется и сравнивается:
  `25398ab9e3d3a26bcce2d37cb27fd2c4c1bc6c9c6119d2153594c43ee0fe4f40`
  (URL `https://freepats.zenvoid.org/Organ/pipe-organ.html` — страница инструмента со строкой
  «Published under the terms of the Creative Commons CC0 1.0 public
  domain dedication»).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `ChurchOrganEmulation-SFZ-20190924/ChurchOrganEmulation-20190924.sfz`.

## Редиректы

`download.url` отвечает `200` напрямую (без редиректа); `download.redirect_hosts` пуст.
SHA-256 архива гарантирует целостность.
