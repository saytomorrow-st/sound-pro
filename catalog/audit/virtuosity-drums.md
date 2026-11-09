# Аудит: Virtuosity Drums

- **id**: `virtuosity-drums`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/virtuosity_drums`; GitHub API
  `license.spdx_id` привязан к тегу `v0.925`).

## Источник

- **source_url**: <https://github.com/sfzinstruments/virtuosity_drums>
- **download.url**: <https://github.com/sfzinstruments/virtuosity_drums/releases/download/v0.925/Virtuosity_Drums_v0.925.zip>

## Архив

- **format**: `zip`
- **size_bytes**: 1227151376
- **sha256**: `c6c5d0fe11a394e94be3146a950c3377ec102cb57d189d5a23cec26183d1963a`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0
  Universal, <https://creativecommons.org/publicdomain/zero/1.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/virtuosity_drums/license?ref=v0.925` (поле `license.spdx_id`, привязано к тегу
  `v0.925`), сравнивается с `CC0-1.0`. `sha256` отсутствует
  (в режиме `metadata` сравнение по SPDX, а не по хэшу текста).

## Сэмплы

- **Форматы**: wav, flac (поддерживаются приложением).
- **Основной `.sfz`**: `Programs/01-basic-kit.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/v0.925/…`) отвечает
`302` на `https://release-assets.githubusercontent.com/…`. В
`download.redirect_hosts` добавлен `.githubusercontent.com` (HTTPS-only,
суффикс). SHA-256 архива гарантирует целостность.
