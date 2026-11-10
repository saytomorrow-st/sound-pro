# Аудит: DSmolken Double Bass

- **id**: `dsmolken-double-bass`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/dsmolken.double-bass`; GitHub API
  `license.spdx_id` привязан к тегу `v1.001`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/dsmolken.double-bass>
- **download.url**: <https://github.com/sfzinstruments/dsmolken.double-bass/releases/download/v1.001/DSmolken.double_bass.v1.001.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 265232156
- **sha256**: `380986bb52ee6b6469d28e9089792a3ed37cbd163fe5a19160bf4f98785e7ccb`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/dsmolken.double-bass/license?ref=v1.001` (поле `license.spdx_id`, привязано к тегу
  `v1.001`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `dsmolken_double_bass/d_smolken_rubner_bass_switched.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.001/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
