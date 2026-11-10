# Аудит: VCSL (Versilian Community Sample Library)

- **id**: `vcsl`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sgossner/VCSL`; GitHub API `license.spdx_id`
  привязан к тегу `v1.2.2-RC`).

## Источник

- **source_url**: <https://github.com/sgossner/VCSL>
- **download.url**: <https://codeload.github.com/sgossner/VCSL/zip/b6e6ac82d22248edee98a0bde185eb9ef6d439ad>

## Архив

- **format**: `zip`
- **size_bytes**: 4047406738
- **sha256**: `d7bd399a124ad36f40233a1288c1ec2499567d67ae34745b1c9c107fbe081698`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sgossner/VCSL/license?ref=v1.2.2-RC` (поле
  `license.spdx_id`, привязано к тегу `v1.2.2-RC`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует (в режиме `metadata` сравнение по SPDX, а не по хэшу
  текста).

## Сэмплы

- **Форматы**: wav (4231), ogg (18), mp3 (6) — поддерживаются приложением.
- **Основной `.sfz`**: отсутствует в манифесте; приложение ищет `.sfz`
  обходом каталога (`find_sfz`).

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
