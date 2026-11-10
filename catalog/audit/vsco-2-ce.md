# Аудит: VSCO-2 Community Edition

- **id**: `vsco-2-ce`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sgossner/VSCO-2-CE`; GitHub API `license.spdx_id`
  привязан к тегу `1.1.0`).

## Источник

- **source_url**: <https://github.com/sgossner/VSCO-2-CE>
- **download.url**: <https://codeload.github.com/sgossner/VSCO-2-CE/zip/28092772094b2d9f1148d84cea97f4545b8c687d>

## Архив

- **format**: `zip`
- **size_bytes**: 2297598334
- **sha256**: `4a4446628df0e1a12aaee58e9f65f8fa7cde51971e961abb1b43083a6d3a8ab7`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sgossner/VSCO-2-CE/license?ref=1.1.0` (поле
  `license.spdx_id`, привязано к тегу `1.1.0`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует.

## Сэмплы

- **Форматы**: wav (3168), mp3 (6) — поддерживаются приложением.
- **Основной `.sfz`**: отсутствует в манифесте; приложение ищет `.sfz`
  обходом каталога (`find_sfz`).

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
