# Аудит: Shinyguitar

- **id**: `shinyguitar`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.shinyguitar`; GitHub API
  `license.spdx_id` привязан к тегу `v1.002`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.shinyguitar>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.shinyguitar/releases/download/v1.002/Karoryfer.Shinyguitar.v1.002.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 368411844
- **sha256**: `23cf4030cbf9ce9e2c84d7cbb1c022fbca124bfff0ad118a14eda7cf6c921d7b`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.shinyguitar/license?ref=v1.002` (поле `license.spdx_id`, привязано к тегу
  `v1.002`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Shinyguitar/Programs/acoustic.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.002/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
