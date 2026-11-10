# Аудит лицензий: кандидаты для «основного набора» (SPEC-076)

- **Дата проверки**: 2026-11-09
- **Метод**: текст лицензии прочитан из первоисточника (файлы `LICENSE`/README
  в репозиториях `sfzinstruments/*`, `sgossner/*`, `freepats/*`, страницы
  freepats.zenvoid.org, vgtrumpet.com). Хэши архивов и размеры **не** считались:
  для включения в каталог каждая запись ещё должна пройти аудит по образцу
  `osiris-piano.md` (SHA-256 реальной загрузкой).
- **Политика**: `bank_library.rs` — запрещены только GPL/AGPL/LGPL и
  share-alike (`is_share_alike`); некоммерческие (NC) разрешены с меткой
  «Только некоммерческое» и предупреждением перед установкой. Банки делятся
  на три класса (`LicenseClass`): «Свободно» (CC0/PD), «Нужно указать автора»
  (CC-BY/MIT и т. п.) и «Только некоммерческое» (NC). CC-BY-3.0/4.0 требуют
  атрибуции. Прочее («не в SPDX») каталог не принимает без решения владельца.
- Пометка «не подтверждено» — текст не найден или не прочитан.

## Подтверждено (SHA-256 реальной загрузкой, 2026-11-09)

Ниже — 19 банков Karoryfer (все CC0-1.0) и 9 дополнительных банков второго
захода (VSCO-2-CE, VCSL, Muldjord Kit, Ixox Flute, Greg Sullivan E-Pianos,
Sonatina Symphonic Orchestra, GTownChurch, AKG R 20 E IRs, Mellotron-SFZ),
добавленные в каталог. Для каждого скачан архив, посчитан SHA-256, проверено
содержимое (`.sfz` + WAV/FLAC/OGG/MP3), а текст лицензии прочитан из
первоисточника. Индивидуальные отчёты — `catalog/audit/<bank_id>.md`.

| Банк (id) | Категория | size_bytes | SHA-256 (первые 16) | Формат сэмплов |
|---|---|---|---|---|
| virtuosity-drums | Drums | 1 227 151 376 | c6c5d0fe11a394e9 | wav, flac |
| shinyguitar | Guitar | 368 411 844 | 23cf4030cbf9ce9e | wav |
| black-and-green-guitars | Guitar | 481 838 512 | ba95221d32fb6386 | wav |
| emilyguitar | Guitar | 103 484 350 | ffef3b2897276382 | wav |
| meatbass | Bass | 255 425 351 | bc053061d4f39fb7 | wav |
| fashionbass | Bass | 316 328 061 | 3beaa956b8ff97d9 | wav |
| sneakybass | Bass | 340 462 855 | df57ef935f2d006d | wav |
| ergo-eub | Bass | 200 612 492 | 21202ec315d0f2cf | wav |
| dsmolken-double-bass | Bass | 265 232 156 | 380986bb52ee6b64 | wav |
| string-cyborgs | Strings | 63 765 812 | 92974bfef97c7dfc | wav |
| bigcat-cello | Strings | 132 605 500 | f340e2afc161de70 | wav |
| bear-sax | Winds | 131 505 201 | 66c28ea361c86943 | wav |
| weresax | Winds | 197 565 664 | 916dcb44b2ec8f03 | wav |
| swirly-drums | Drums | 868 594 745 | dbeb1ad04052da1b | wav |
| unruly-drums | Drums | 676 343 466 | 8d8d807557008865 | wav, flac |
| big-rusty-drums | Drums | 619 600 370 | d4a9990acd19376d | wav, flac |
| caveman-cosmonaut | Synth | 97 546 345 | e494eb1656b9d637 | wav |
| cowsynth | Synth | 14 072 509 | 2c80a928db69280d | wav |
| merry-orks | Voice | 40 439 723 | c5151c24a57fddfc | wav |
| vcsl | Strings | 4 047 406 738 | d7bd399a124ad36f | wav, ogg, mp3 |
| vsco-2-ce | Strings | 2 297 598 334 | 4a4446628df0e1a1 | wav, mp3 |
| muldjord-kit | Drums | 345 968 791 | 90a1e2d8c749a67d | flac |
| ixox-flute | Winds | 9 526 829 | e5fb9116de784e16 | flac |
| greg-sullivan-e-pianos | Piano | 19 250 842 | b6cb93e68f0d67ef | flac |
| gtownchurch | Percussion | 457 439 813 | 6347d07cd88ddb2f | wav |
| sonatina-symphonic-orchestra | Strings | 1 377 249 014 | 1791e3a5f8ceb6b7 | flac, wav |
| akg-r20e-ir | ImpulseResponses | 7 187 061 | a4b08b094c006c1a | wav |
| mellotron-sfz | Synth | 510 946 413 | 6eb52869300463a9 | wav |

## Сводная таблица

| Банк | Первоисточник текста лицензии | SPDX | В каталог | Атрибуция |
|---|---|---|---|---|
| Greg Sullivan E-Pianos | `sfzinstruments/GregSullivan.E-Pianos` → `LICENSE` (CC Legal Code, Attribution 3.0) | `CC-BY-3.0` | да | да |
| Sonatina Symphonic Orchestra | `peastman/sso` → `LICENSE`, README | нет в SPDX (CC Sampling Plus 1.0) | да (некоммерческое) | да |
| Virtual Playing Orchestra 3.2 | `open-soundfonts/Virtual_Playing_Orchestra_3` → `LICENSE` (GPL-3.0), `Documentation/license.htm` | `GPL-3.0` + части `CC-BY-SA` | нет | — |
| GeneralUser GS 2.0.3 | `mrbumpy409/GeneralUser-GS` → `documentation/LICENSE.txt` | нет (собственная «License v2.0») | нет (условно) | не требуется текстом |
| Maestro Concert Grand | `sfzinstruments/MatsHelgesson.MaestroConcertGrandPiano` → `mcgreadme.txt` | нет (proprietary) | нет | — |
| Church Steinway | не подтверждено | не подтверждено | нет | не подтверждено |
| GTownChurch | `sfzinstruments/GTownChurchSamplingProject` → README | нет в SPDX (CC Sampling Plus 1.0) | да (некоммерческое) | да |
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
| AKG R 20 E Spring Reverb IRs | `DomMcsweeney/AKG-R-20-E-Spring-Reverb-Impulse-Responses` → `LICENSE` | `MIT` | да | да |
| Mellotron-SFZ | `ExistentiaVirae/Mellotron-SFZ` → README (цитата владельца) | нет в SPDX (только некоммерческое) | да (некоммерческое) | да |

## Подробности и оговорки

URL текстов (все прочитаны):
- Greg Sullivan: <https://github.com/sfzinstruments/GregSullivan.E-Pianos/blob/master/LICENSE>
  (README ссылается на <http://creativecommons.org/licenses/by/3.0/>). Атрибуция: Greg Sullivan.
  GitHub API отдаёт `NOASSERTION`, но текст — CC BY 3.0 Unported.
- Sonatina: <https://github.com/peastman/sso/blob/master/LICENSE>: «Creative Commons
  Sampling Plus 1.0». README SSO также признаёт, что происхождение части
  старых сэмплов неизвестно. Sampling Plus разрешает копирование всего
  произведения только для некоммерческих целей → класс «Только
  некоммерческое». **Включён** по решению владельца (некоммерческое,
  предупреждение перед установкой).
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
  только некоммерческое → **включён** как «Только некоммерческое» (решение
  владельца).
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
и Muldjord Kit (CC-BY-4.0), VSCO-2-CE и VCSL (CC0), Sonatina и GTownChurch
(Sampling Plus, некоммерческое), AKG R 20 E IRs (MIT), Mellotron-SFZ
(некоммерческое, цитата владельца). FreePats Harp/Timpani/Pipe Organ/Synth Pad
Choir/Synth Strings (CC0) годятся по лицензии, но их архивы `.tar.xz`/`.7z` не
поддерживаются загрузчиком — **не включены**. Условно: Splendid Grand,
Wavestate Pads, GeneralUser GS. Не годятся: VPO (GPL/SA), Maestro
(proprietary), Northern Trumpets (коммерческая). Не подтверждены:
Church Steinway, OrgueEglise.
