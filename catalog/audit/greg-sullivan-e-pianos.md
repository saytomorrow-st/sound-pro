# Аудит: Greg Sullivan E-Pianos

- **id**: `greg-sullivan-e-pianos`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/GregSullivan.E-Pianos` — CC BY 3.0
  Unported; GitHub API отдаёт `NOASSERTION`, поэтому лицензия фиксируется по
  хэшу текста).

## Источник

- **source_url**: <https://github.com/sfzinstruments/GregSullivan.E-Pianos>
- **download.url**: <https://codeload.github.com/sfzinstruments/GregSullivan.E-Pianos/zip/8c3e581acda3594b553948ff0222d4f84a698376>

## Архив

- **format**: `zip`
- **size_bytes**: 19250842
- **sha256**: `b6cb93e68f0d67efa8d453c75607e357e0fdbd29cc6c36a25ccf6091f492c590`

## Лицензия

- **SPDX**: `CC-BY-3.0`
- **attribution_required**: `true`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC
  Attribution 3.0 Unported, <https://creativecommons.org/licenses/by/3.0/legalcode>).
- **license_check**: mode `text-hash` — SHA-256 нормализованного текста
  legalcode фиксируется и сравнивается:
  `11dbf7126ae9794e4a7dc54334f3c82a67efcf22df888d396dc2ed707b2a3f9b`
  (URL `https://creativecommons.org/licenses/by/3.0/legalcode.txt`).

## Сэмплы

- **Форматы**: flac (172) — поддерживаются приложением.
- **Основной `.sfz`**: `GregSullivan.E-Pianos-master/CP80/CP80.sfz`.

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
