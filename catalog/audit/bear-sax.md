# Аудит: Bear Sax

- **id**: `bear-sax`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.bear-sax`; GitHub API
  `license.spdx_id` привязан к тегу `v1.004`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.bear-sax>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.bear-sax/releases/download/v1.004/Karoryfer.Bear_Sax.v1.004.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 131505201
- **sha256**: `66c28ea361c86943b2e2bcda6ca48cf105fec4f60a8f40e7c6bbf584be45c654`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.bear-sax/license?ref=v1.004` (поле `license.spdx_id`, привязано к тегу
  `v1.004`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Bear_Sax/Programs/1-solo-mono.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.004/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
