# Аудит: Mellotron-SFZ

- **id**: `mellotron-sfz`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (README в репозитории `ExistentiaVirae/Mellotron-SFZ`; GitHub API
  `license.spdx_id` не задан, поэтому лицензия фиксируется по хэшу текста
  README и цитате владельца).

## Источник

- **source_url**: <https://github.com/ExistentiaVirae/Mellotron-SFZ>
- **download.url**: <https://codeload.github.com/ExistentiaVirae/Mellotron-SFZ/zip/e192f8a257901e1dc854016c1008522b363b15e2>

## Архив

- **format**: `zip`
- **size_bytes**: 510946413
- **sha256**: `6eb52869300463a91957759f5ba729ae26e717456150efc02d7d6571a6d95521`

## Лицензия

- **SPDX**: `LicenseRef-NC-Mellotron`
- **attribution_required**: `false`
- **Цитата владельца (из README)**: «…they are free to use, but cannot be used
  in any commercial, for profit software.» — условие: только некоммерческое
  использование (класс «Только некоммерческое»). Воспроизводится в поле
  `attribution` манифеста.
- **license_check**: mode `text-hash` — SHA-256 нормализованного README
  фиксируется и сравнивается:
  `81b9d37f5995b0a23887b8295e893cf27a3aff11d500554a1bc33b27c4a8ea9e`
  (URL `…/ExistentiaVirae/Mellotron-SFZ/e192f8a25…/README.md`).

## Сэмплы

- **Форматы**: wav (805) — поддерживаются приложением.
- **Основной `.sfz`**: `Mellotron-SFZ-main/Mellotron/Orchestra/Orchestra.sfz`.

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
