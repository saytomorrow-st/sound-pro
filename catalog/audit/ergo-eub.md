# Аудит: Ergo EUB

- **id**: `ergo-eub`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.ergo`; GitHub API
  `license.spdx_id` привязан к тегу `v1.001`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.ergo>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.ergo/releases/download/v1.001/Karoryfer.Ergo_EUB.v1.001.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 200612492
- **sha256**: `21202ec315d0f2cffabc7453dafed24451d27657929d4d90860c85f55e42e049`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.ergo/license?ref=v1.001` (поле `license.spdx_id`, привязано к тегу
  `v1.001`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Ergo_EUB/ergo_arco.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.001/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
