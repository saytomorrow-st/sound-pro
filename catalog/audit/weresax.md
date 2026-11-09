# Аудит: Weresax

- **id**: `weresax`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/karoryfer.weresax`; GitHub API
  `license.spdx_id` привязан к тегу `v1.003`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/karoryfer.weresax>
- **download.url**: <https://github.com/sfzinstruments/karoryfer.weresax/releases/download/v1.003/Karoryfer.Weresax.v.1.003.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 197565664
- **sha256**: `916dcb44b2ec8f03a3578c374dd1f1fdaa43e9e0264d921f65da2a339539d2ba`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/karoryfer.weresax/license?ref=v1.003` (поле `license.spdx_id`, привязано к тегу
  `v1.003`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `Weresax/Programs/Sax.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v1.003/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
