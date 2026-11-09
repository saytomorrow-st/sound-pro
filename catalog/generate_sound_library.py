#!/usr/bin/env python3
"""Generate catalog/sound-library.json from the Apple Logic catalog metadata.

Reads the tree structure (names/titles only) from a local Apple catalog JSON and
emits the Sound Pro data file: top-level groups keep the screenshot's Russian
titles, subgroups get Russian labels (proper names stay in Latin), sizes are
zero except where a group maps to our own bank catalog (the piano group).

The Apple file path is passed as argv[1]; sizes are NOT copied (Apple sizes are
theirs, not ours). This is structure/metadata only — no Apple audio is
downloaded or redistributed.
"""
import json
import sys

SRC = sys.argv[1]

# Top-level groups: Apple id -> (stable Sound Pro id, Russian title, installed?)
GROUPS = [
    ("CoreContent10", "core", "Основные звуки и инструменты", True),
    ("SoundPacks", "sound-packs", "Звуковые наборы", False),
    ("Producer Packs", "producer-packs", "Продюсерские наборы", False),
    ("Alchemy", "alchemy", "Библиотеки Alchemy", False),
    ("InstrumentsBass", "bass", "Бас", False),
    ("InstrumentsCinematic", "cinematic", "Кинематографичные", False),
    ("DrumKits", "drum-kit", "Ударная установка", False),
    ("ElectronicDrums", "electronic-drum-kit", "Электронная ударная установка", False),
    ("InstrumentsGuitar", "guitar", "Гитара", False),
    ("InstrumentsMallet", "mallets", "Мэллеты", False),
    ("OrchestralInstruments", "orchestral", "Оркестровые", False),
    ("Percussion", "percussion", "Перкуссия", False),
    ("InstrumentsPiano", "piano", "Фортепиано", False),
    ("InstrumentsStudioHorns", "studio-horns", "Студийные духовые", False),
    ("InstrumentsStudioStrings", "studio-strings", "Студийные струнные", False),
    ("InstrumentsMellotron", "vintage-mellotron", "Винтажный меллотрон", False),
    ("WorldInstruments", "world", "Мир", False),
    ("AppleLoops", "loops", "Лупы", False),
    ("ImpulseResponses", "reverbs", "Реверберации", False),
    ("Legacy", "legacy", "Устаревшее и совместимость", False),
]

# Russian titles for subgroup node names (keyed by Apple name).
# Proper names (producers, brand pack names) stay in Latin.
SUB_TITLES = {
    # Sound Packs
    "SoundPack 8-Bit Legends": "8-битные легенды (Чиптюн)",
    "SoundPack Alpha Waves": "Альфа-волны (Современный эмбиент)",
    "SoundPack Backlight Bounce": "Backlight Bounce (Slap House)",
    "SoundPack Beat Tape": "Beat Tape (Хип-хоп)",
    "SoundPack Chromium Fray": "Chromium Fray (Bass House)",
    "SoundPack Dancefloor Rush": "Dancefloor Rush (Драм-н-бейс)",
    "SoundPack Edges and Angles": "Edges and Angles (Future Bass)",
    "SoundPack Flex and Flow": "Flex and Flow (Chill Hip Hop)",
    "SoundPack Gozadera Latina": "Gozadera Latina (Реггетон-поп)",
    "SoundPack Hybrid Textures": "Гибридные текстуры (Sample Alchemy)",
    "SoundPack Keyboard Collection": "Коллекция клавишных",
    "SoundPack Magnetic Imperfections": "Magnetic Imperfections",
    "SoundPack Modular Melodies": "Модульные мелодии (Синтезаторы)",
    "SoundPack Modular Rhythms": "Модульные ритмы (Синтезаторные ударные)",
    "SoundPack Oblique Structures": "Oblique Structures (Техно)",
    "SoundPack Prismatica": "Prismatica (Ню-диско)",
    "SoundPack Skyline Heat": "Skyline Heat (Хип-хоп)",
    "SoundPack Toy Box": "Коробка игрушек (Звуковые эффекты)",
    "SoundPack Transition Effects": "Эффекты переходов",
    "SoundPack Ultimate 808s": "Ultimate 808s",
    "SoundPack Vintage Drum Machines": "Винтажные драм-машины",
    "SoundPack Vision and Verse": "Vision and Verse (Boom Bap Hip Hop)",
    "SoundPack Vox Melodics": "Vox Melodics (Вокальные лупы)",
    # Alchemy
    "Atmospheric": "Атмосферные",
    "Basic Additions": "Базовые дополнения",
    "Cinematic": "Кинематографичные",
    "Dance": "Танцевальные",
    "Modern Songwriter": "Современный автор",
    "Modern Synth": "Современный синтезатор",
    "Textures and Effects": "Текстуры и эффекты",
    "Vintage Synth": "Винтажный синтезатор",
    "Visions": "Видения",
    # Drum kits
    "AcousticDrums": "Стерео-ударные",
    "ProducerDrumKits": "Многоканальные ударные",
    "StereoDrumKitsSongwriter": "Наборы «Автор»",
    "StereoDrumKitsAlternative": "Наборы «Альтернатива»",
    "StereoDrumKitsRock": "Наборы «Рок»",
    "StereoDrumKitsRnB": "Наборы «R&B»",
    "ProducerClassicSixtiesKit": "Набор «Шестидесятые»",
    "ProducerPawnShopKit": "Набор «Ломбард»",
    "ProducerBirchKit": "Набор «Берёза»",
    "ProducerStadiumKit": "Набор «Стадион»",
    "ProducerSeventiesPlexiKit": "Набор «Семидесятые Plexi»",
    "ProducerTightMapleKit": "Набор «Плотный клён»",
    "ProducerStudioKit": "Набор «Студия»",
    "ProducerModernMapleKit": "Набор «Современный клён»",
    "ProducerVintageBrushKit": "Набор «Винтажные щётки»",
    "ProducerVintageRodsKit": "Набор «Винтажные прутья»",
    "ProducerVintageSticksKit": "Набор «Винтажные палочки»",
    # Orchestral
    "InstrumentsOrchestralBrass": "Медные духовые",
    "InstrumentsOrchestralChoir": "Хор",
    "InstrumentsOrchestralHarp": "Арфа",
    "InstrumentsOrchestralKeyboard": "Клавишные",
    "InstrumentsOrchestralPercussion": "Ударные",
    "InstrumentsOrchestralPipeOrgan": "Орган",
    "InstrumentsOrchestralStrings": "Струнные",
    "InstrumentsOrchestralWoodwinds": "Деревянные духовые",
    # World
    "InstrumentsWorldKeyboards": "Клавишные",
    "InstrumentsWorldPercussion": "Ударные",
    "InstrumentsWorldStringed": "Струнные",
    "InstrumentsWorldVoice": "Вокал",
    "InstrumentsWorldWoodwind": "Духовые",
    # Apple Loops
    "AppleLoops8-BitLegends": "8-битные легенды",
    "AppleLoopsBassPlayer": "Бас-гитарист",
    "AppleLoopsBluesGarage": "Блюз-гараж",
    "AppleLoopsChillwave": "Чиллвейв",
    "AppleLoopsChineseTraditional": "Китайская традиционная",
    "AppleLoopsDeepHouse": "Дип-хаус",
    "AppleLoopsDiscoFunk": "Диско-фанк",
    "AppleLoopsDrummer": "Ударник",
    "AppleLoopsDubstep": "Дабстеп",
    "AppleLoopsElectroHouse": "Электро-хаус",
    "AppleLoopsHipHop": "Хип-хоп",
    "AppleLoopsIndieDisco": "Инди-диско",
    "AppleLoopsKeyboardPlayer": "Клавишник",
    "AppleLoopsModernRnB": "Современный R&B",
    "AppleLoopsStepSequencer": "Степ-секвенсор",
    "AppleLoopsStudioHorns": "Студийные духовые",
    "AppleLoopsStudioStrings": "Студийные струнные",
    "AppleLoopsTechHouse": "Тех-хаус",
    "AppleLoopsVintageBreaks": "Винтажные брейки",
    # Reverbs
    "StereoIRs": "Стерео",
    "SurroundIRs": "Объёмный звук",
    # Legacy
    "LogicPro10LegacyContent": "Звуки и инструменты совместимости Logic Pro 10",
    "LogicPro10InstrumentsBass": "Бас",
    "LogicPro10AcousticDrums": "Стерео-ударные",
    "LogicPro10StereoDrumKitsSongwriter": "Наборы «Автор»",
    "LogicPro10StereoDrumKitsAlternative": "Наборы «Альтернатива»",
    "LogicPro10StereoDrumKitsRock": "Наборы «Рок»",
    "LogicPro10StereoDrumKitsRnB": "Наборы «R&B»",
    "LogicPro10InstrumentsPiano": "Фортепиано",
    "Logic9LegacyContent": "Звуки и инструменты совместимости Logic Studio 2",
    "GarageBand11Content": "Звуки и инструменты GarageBand 11",
    "JamPack1": "Jam Pack 1",
    "JamPackRemixTools": "Jam Pack Remix Tools",
    "JamPackRhythmSection": "Jam Pack Rhythm Section",
    "JamPackSymphonyOrchestra": "Jam Pack Symphony Orchestra",
    "JamPackWorldMusic": "Jam Pack WorldMusic",
    "JamPackVoices": "Jam Pack Voices",
    "JamPack1Instruments": "Инструменты",
    "JamPack1AppleLoops": "Apple Loops",
    "JamPackRemixToolsInstruments": "Инструменты",
    "JamPackRemixToolsAppleLoops": "Apple Loops",
    "JamPackRhythmSectionInstruments": "Инструменты",
    "JamPackRhythmSectionAppleLoops": "Apple Loops",
    "JamPackSymphonyOrchestraInstruments": "Инструменты",
    "JamPackSymphonyOrchestraAppleLoops": "Apple Loops",
    "JamPackWorldMusicInstruments": "Инструменты",
    "JamPackWorldMusicAppleLoops": "Apple Loops",
    "JamPackVoicesInstruments": "Инструменты",
    "JamPackVoicesAppleLoops": "Apple Loops",
}

# Sound Pro bank ids attached to the piano group (from our own bank catalog).
PIANO_BANKS = ["salamander-grand-v3", "osiris-piano"]

def slugify(name):
    out = []
    for ch in name:
        if ch.isalnum():
            out.append(ch.lower())
        elif ch in " -":
            out.append("-")
    s = "".join(out)
    return "-".join(p for p in s.split("-") if p)

def build_node(group):
    title = SUB_TITLES.get(group["name"], group["title"])
    node = {"id": slugify(group["name"]), "title": title}
    subs = group.get("sub", [])
    if subs:
        node["sub"] = [build_node(s) for s in subs]
    return node

def main():
    with open(SRC, encoding="utf-8") as f:
        apple = json.load(f)
    by_name = {g["name"]: g for g in apple}

    groups = []
    for apple_id, sp_id, title, installed in GROUPS:
        g = by_name[apple_id]
        node = {"id": sp_id, "title": title}
        if installed:
            node["installed"] = True
        if sp_id == "piano":
            node["banks"] = PIANO_BANKS
        subs = g.get("sub", [])
        if subs:
            node["sub"] = [build_node(s) for s in subs]
        groups.append(node)

    doc = {"schema": 1, "groups": groups}
    with open("catalog/sound-library.json", "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print("wrote catalog/sound-library.json:", len(groups), "groups")

if __name__ == "__main__":
    main()
