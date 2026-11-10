# Аудит: FreePats Concert Harp

- **id**: `freepats-harp`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://freepats.zenvoid.org/OrchestralStrings/harp.html>
- **download.url**: <https://freepats.zenvoid.org/OrchestralStrings/ConcertHarp/ConcertHarp-SFZ-20200702.tar.xz>

## Архив

- **format**: `tar.xz`
- **size_bytes**: 9502812
- **sha256**: `e7dacbb92ff11bbb2eff97fe837e370341ef175e6b401309f05b623da87e3a1e`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Published under the terms of Creative Commons CC0 public domain dedication»
- **license_check**: mode `text-hash` — SHA-256 нормализованного текста
  страницы фиксируется и сравнивается:
  `06b9f775dda375165d81b8eec0e80134ac5d69d0a94d075ff77afb0a9f8af1b9`
  (URL `https://freepats.zenvoid.org/OrchestralStrings/harp.html` — страница инструмента со строкой
  «Published under the terms of the Creative Commons CC0 1.0 public
  domain dedication»).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `ConcertHarp-SFZ-20200702/ConcertHarp-20200702.sfz`.

## Редиректы

`download.url` отвечает `200` напрямую (без редиректа); `download.redirect_hosts` пуст.
SHA-256 архива гарантирует целостность.
