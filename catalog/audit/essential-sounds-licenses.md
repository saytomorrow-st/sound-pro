# Аудит лицензий: кандидаты для «основного набора» (SPEC-076)

- **Дата проверки**: 2026-11-09
- **Метод**: текст лицензии прочитан из первоисточника (файлы `LICENSE`/README
  в репозиториях `sfzinstruments/*`, `sgossner/*`, `freepats/*`, страницы
  freepats.zenvoid.org, vgtrumpet.com). Хэши архивов и размеры **не** считались:
  для включения в каталог каждая запись ещё должна пройти аудит по образцу
  `osiris-piano.md` (SHA-256 реальной загрузкой).
- **Политика**: `bank_library.rs` — запрещены `NC`/`ND` (`is_forbidden`),
  share-alike и GPL/AGPL/LGPL (`is_share_alike`); CC-BY-3.0/4.0 требуют
  атрибуции. Прочее («не в SPDX») каталог не принимает без решения владельца.
- Пометка «не подтверждено» — текст не найден или не прочитан.

## Сводная таблица

| Банк | Первоисточник текста лицензии | SPDX | В каталог | Атрибуция |
|---|---|---|---|---|
| Greg Sullivan E-Pianos | `sfzinstruments/GregSullivan.E-Pianos` → `LICENSE` (CC Legal Code, Attribution 3.0) | `CC-BY-3.0` | да | да |
| Sonatina Symphonic Orchestra | `peastman/sso` → `LICENSE`, README | нет в SPDX (CC Sampling Plus 1.0) | нет | да |
| Virtual Playing Orchestra 3.2 | `open-soundfonts/Virtual_Playing_Orchestra_3` → `LICENSE` (GPL-3.0), `Documentation/license.htm` | `GPL-3.0` + части `CC-BY-SA` | нет | — |
| GeneralUser GS 2.0.3 | `mrbumpy409/GeneralUser-GS` → `documentation/LICENSE.txt` | нет (собственная «License v2.0») | нет (условно) | не требуется текстом |
| Maestro Concert Grand | `sfzinstruments/MatsHelgesson.MaestroConcertGrandPiano` → `mcgreadme.txt` | нет (proprietary) | нет | — |
| Church Steinway | не подтверждено | не подтверждено | нет | не подтверждено |
| GTownChurch | `sfzinstruments/GTownChurchSamplingProject` → README | нет в SPDX (CC Sampling Plus 1.0) | нет | да |
| OrgueEglise | не подтверждено (README и `.sfz` без лицензии) | не подтверждено | нет | не подтверждено |
| Northern Trumpets | vgtrumpet.com/product/northern-trumpets-sfz/ | коммерческая | нет | — |
| VCSL | `sgossner/VCSL` → `LICENSE` | `CC0-1.0` | да | нет |
| VSCO-2-CE | `sgossner/VSCO-2-CE` → `LICENSE` | `CC0-1.0` | да | нет |
| FreePats: Harp (Concert Harp) | freepats.zenvoid.org/OrchestralStrings/harp.html | `CC0-1.0` | да | нет |
| FreePats: Timpani | `freepats/timpani` → `LICENSE.txt`, README | `CC0-1.0` | да | нет |
| FreePats: Pipe Organ | freepats.zenvoid.org/Organ/pipe-organ.html | `CC0-1.0` | да | нет |
| FreePats: Choir (Synth Pad Choir) | `freepats/synth-pad-choir` → `LICENSE.txt`, README | `CC0-1.0` | да (синтетика) | нет |
| FreePats: Strings (Synth Strings 1/2) | `freepats/synth-strings-1/2` → `LICENSE.txt` | `CC0-1.0` | да (синтетика) | нет |
| Karoryfer: Shinyguitar, Growlybass, Swagbass, Bigcat Cello, War Tuba, Gogodze Phu Vol II | `sfzinstruments/karoryfer.*` → `LICENSE` (CC0 1.0 Universal) | `CC0-1.0` | да | нет |
| Ixox Flute | `sfzinstruments/Ixox.Flute` → `LICENSE`, README | `CC-BY-4.0` | да | да |
| Muldjord Kit | `sfzinstruments/DrumGizmo.MuldjordKit` → `LICENSE`, README | `CC-BY-4.0` | да | да |
| Splendid Grand Piano | `sfzinstruments/SplendidGrandPiano` → README | public domain (SPDX не назначен) | условно | нет |
| Wavestate Pads | `fdivitto/KorgWavestate-SoundLibrary` → `LICENSE` (CC0); drolez.com: «CC0-1.0» | `CC0-1.0` (источник пака не подтверждён) | условно | нет |

## Подробности и оговорки

URL текстов (все прочитаны):
- Greg Sullivan: <https://github.com/sfzinstruments/GregSullivan.E-Pianos/blob/master/LICENSE>
  (README ссылается на <http://creativecommons.org/licenses/by/3.0/>). Атрибуция: Greg Sullivan.
  GitHub API отдаёт `NOASSERTION`, но текст — CC BY 3.0 Unported.
- Sonatina: <https://github.com/peastman/sso/blob/master/LICENSE>: «Creative Commons
  Sampling Plus 1.0». README SSO также признаёт, что происхождение части
  старых сэмплов неизвестно. Sampling Plus разрешает копирование всего
  произведения только для некоммерческих целей → по смыслу NC, но в `is_forbidden` не
  попадёт по подстроке; **отклонить вручную**. Полный legalcode Sampling+ я не открывал,
  вывод опирается на резюме в README `GTownChurch` (оно дословно повторяет условия).
- VPO 3.2: проверен клон `open-soundfonts/Virtual_Playing_Orchestra_3`
  (`LICENSE` = GPL-3.0; `Documentation/license.htm` перечисляет CC-BY-SA 3.0/4.0
  для Philharmonia, No Budget Orchestra и др.). Первоисточник
  virtualplaying.com не открывался, номер версии 3.2 не подтверждён → не подтверждено.
- GeneralUser GS 2.0.3: <https://github.com/mrbumpy409/GeneralUser-GS/blob/main/documentation/LICENSE.txt>.
  Свободное использование и распространение разрешены, но автор прямо пишет, что
  происхождение всех сэмплов не может гарантировать. Не SPDX, формат SF2 (не SFZ).
- Maestro: `mcgreadme.txt`: «All rights reserved… may not modify and spread
  without the author's written permission». Репозиторий sfz — «published with
  permission via e-mail» (разрешение не публичное) → не годится.
- Church Steinway: <https://www.pianobook.co.uk/library/church-steinway/> —
  страница не отдала текст лицензии (динамическая), в `instruments.yml`
  `sfzinstruments.github.io` поле `license` отсутствует. Не подтверждено.
- GTownChurch: README, раздел License: Creative Commons Sampling Plus 1.0
  (<https://creativecommons.org/licenses/sampling+/1.0/>), распространение
  только некоммерческое → отклонить.
- OrgueEglise: README и `.sfz` — только «converted by S. Christian Collins»,
  лицензии нет; в `instruments.yml` записи нет. Не подтверждено.
- Northern Trumpets: на странице продукта цена €16.87 (коммерческая), хотя
  `instruments.yml` помечает «Free» → отклонить.
- VCSL: README — «CC0 … no credit». Оригинал — `sgossner/VCSL` (в поиске по
  GitHub другого оригинала не найдено). В составе есть `Peter Eastman` SFZ-генератор (CC0).
- VSCO-2-CE: `LICENSE` — CC0 1.0 Universal, GitHub API `CC0-1.0`.
- FreePats: у каждого банка своя лицензия; на странице Harp упомянуто «CC BY-NC»,
  но только для **фотографии**, сэмплы CC0. Страницы `Piano/acoustic-grand-piano`
  упоминают одновременно CC0 и Attribution 3.0 — для него состав не подтверждён.
  `Percussion/acoustic-drum-kit` — Attribution 4.0 (Muldjord). Настоящего хора
  у FreePats нет (только синтетический `synth-pad-choir`).
- Karoryfer: в каждом репозитории `LICENSE` — CC0 1.0 Universal (проверены
  Shinyguitar, Growlybass, Swagbass, Bigcat Cello, War Tuba, Gogodze Phu Vol II).
- Splendid Grand: README «Public Domain samples by AKAI… released as public
  domain in early 2000». Авторская декларация без юридического текста; SPDX нет
  → в каталоге потребуется `CC0-1.0` заявка владельца или исключение. Условно.
- Wavestate Pads: `instruments.yml` указывает автора SHLD, CC0-1.0, URL
  drolez.com; на странице drolez прямо «licensed under CC0-1.0». Репозиторий
  с файлами пака не найден → прямой URL загрузки не подтверждён.

## Итог по политике каталога

Годятся: Greg Sullivan (CC-BY-3.0), Karoryfer (шесть банков, CC0), Ixox Flute
и Muldjord Kit (CC-BY-4.0), VSCO-2-CE и VCSL (CC0), FreePats Harp/Timpani/Pipe
Organ/Synth Pad Choir/Synth Strings (CC0). Условно: Splendid Grand, Wavestate Pads,
GeneralUser GS. Не годятся: Sonatina, GTownChurch (Sampling Plus), VPO (GPL/SA),
Maestro (proprietary), Northern Trumpets (коммерческая). Не подтверждены:
Church Steinway, OrgueEglise.
