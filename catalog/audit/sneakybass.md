# Аудит: Sneakybass

- **id**: `sneakybass`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.sneakybass`; GitHub API
  `license.spdx_id` привязан к тегу `v1.000`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.sneakybass>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.sneakybass/releases/download/v1.000/Sneakybass_v1.000.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 340462855
- **sha256**: `df57ef935f2d006d83f898f615893cd2e9f93d00d01fb03921c54cd6dd5014dd`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.sneakybass/license?ref=v1.000` (поле `license.spdx_id`, привязано к тегу
  `v1.000`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Sneakybass/Programs/01-sneakybass_keyswitch.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.000/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
