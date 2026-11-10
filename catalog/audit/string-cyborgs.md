# Аудит: String Cyborgs

- **id**: `string-cyborgs`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.string-cyborgs`; GitHub API
  `license.spdx_id` привязан к тегу `v1.001`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.string-cyborgs>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.string-cyborgs/releases/download/v1.001/Karoryfer.String_Cyborgs.v1.001.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 63765812
- **sha256**: `92974bfef97c7dfc0e50abb9a262d6d39b8551efa5750b5fce5a62cb532da3e2`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.string-cyborgs/license?ref=v1.001` (поле `license.spdx_id`, привязано к тегу
  `v1.001`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `String_Cyborgs/Programs/Blackheart.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.001/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
