# Аудит: Cowsynth

- **id**: `cowsynth`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.cowsynth`; GitHub API
  `license.spdx_id` привязан к тегу `v1.001`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.cowsynth>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.cowsynth/releases/download/v1.001/Karoryfer.Cowsynth.v1.001.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 14072509
- **sha256**: `2c80a928db69280d3f8f4d066383c9f6adc0d8ba086c6f1a1abe2c5eaa5fc239`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.cowsynth/license?ref=v1.001` (поле `license.spdx_id`, привязано к тегу
  `v1.001`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Cowsynth/cowsynth_asthmatic_organ.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.001/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
