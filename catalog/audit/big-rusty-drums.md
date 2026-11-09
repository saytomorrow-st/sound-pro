# Аудит: Big Rusty Drums

- **id**: `big-rusty-drums`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.big-rusty-drums`; GitHub API
  `license.spdx_id` привязан к тегу `v1.100`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.big-rusty-drums>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.big-rusty-drums/releases/download/v1.100/Big_Rusty_Drums_1100.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 619600370
- **sha256**: `d4a9990acd19376d91ce446dae415c81428728b5adebb6a88eddbb3a6aac8744`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.big-rusty-drums/license?ref=v1.100` (поле `license.spdx_id`, привязано к тегу
  `v1.100`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav, flac (поддерживаются приложением).
- **Основной `.sfz`**: `Programs/01-full.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.100/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
