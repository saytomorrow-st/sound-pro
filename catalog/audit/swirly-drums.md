# Аудит: Swirly Drums

- **id**: `swirly-drums`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.swirly-drums`; GitHub API
  `license.spdx_id` привязан к тегу `v1.104`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.swirly-drums>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.swirly-drums/releases/download/v1.104/Swirly.Drums_1104.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 868594745
- **sha256**: `dbeb1ad04052da1bada490ced8cc7d9fdd3b21ea90f7c6020269968b70f836c3`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.swirly-drums/license?ref=v1.104` (поле `license.spdx_id`, привязано к тегу
  `v1.104`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Programs/Basic_kit.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.104/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
