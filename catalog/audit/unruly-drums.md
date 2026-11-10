# Аудит: Unruly Drums

- **id**: `unruly-drums`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.unruly-drums`; GitHub API
  `license.spdx_id` привязан к тегу `v1.100`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.unruly-drums>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.unruly-drums/releases/download/v1.100/Unruly_Drums_1100.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 676343466
- **sha256**: `8d8d8075570088658cfce5de6cc6df1fa1340cac9ec808da130e19b1463b1f90`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.unruly-drums/license?ref=v1.100` (поле `license.spdx_id`, привязано к тегу
  `v1.100`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav, flac (поддерживаются приложением).
- **Основной `.sfz`**: `Programs/01-kit-sticks.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.100/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
