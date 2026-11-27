"""Build Flower's offline word-diversity table from the pinned PLOS supplement."""

import argparse
import csv
import hashlib
import io
from pathlib import Path
import struct
import zipfile


ROOT = Path(__file__).resolve().parents[1] / "data" / "spoken_ranking"
SOURCE = ROOT / "plos-subtlex-ch-s002.zip"
OUTPUT = ROOT / "word-diversity.bin"
SOURCE_SHA = "cced9cb382914b93956a24fd06c10de709d351ff4501ac94c9ab136004421f07"
WORD_FORM_SHA = "086536450b1f77d0c7ff3ac0fc8375897162ace807d3167bec48b4c493434077"
LENGTHS = range(2, 9)


def build(source: Path) -> tuple[bytes, int]:
    data = source.read_bytes()
    if hashlib.sha256(data).hexdigest() != SOURCE_SHA:
        raise ValueError("SUBTLEX-CH source archive failed SHA-256 verification")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        if archive.namelist().count("SUBTLEX-CH-WF") != 1:
            raise ValueError("Expected exactly one SUBTLEX-CH-WF table")
        data = archive.read("SUBTLEX-CH-WF")
    if hashlib.sha256(data).hexdigest() != WORD_FORM_SHA:
        raise ValueError("SUBTLEX-CH-WF failed SHA-256 verification")
    rows = csv.reader(io.StringIO(data.decode("gb18030")), delimiter="\t")
    next(rows)
    next(rows)
    if next(rows) != ["Word", "WCount", "W/million", "logW", "W-CD", "W-CD%", "logW-CD"]:
        raise ValueError("Unexpected SUBTLEX-CH-WF columns")

    groups: dict[int, dict[str, int]] = {width: {} for width in LENGTHS}
    total = 0
    for word, count, _, _, diversity, _, _ in rows:
        count, diversity = int(count), int(diversity)
        if not 1 <= diversity <= min(count, 6243):
            raise ValueError(f"Invalid SUBTLEX-CH context count: {word!r}")
        total += 1
        if (len(word) in groups and diversity >= 2
                and all("\u4e00" <= char <= "\u9fff" for char in word)):
            group = groups[len(word)]
            if word in group:
                raise ValueError(f"Duplicate SUBTLEX-CH word: {word!r}")
            group[word] = diversity
    if total != 99121:
        raise ValueError(f"Unexpected source row count: {total}")

    # Eight little-endian offsets, including a final sentinel. For each length
    # append sorted fixed-width UTF-8 words followed by a little-endian u16 CD.
    offsets = [4 * (len(LENGTHS) + 1)]
    body = bytearray()
    for group in groups.values():
        for word, diversity in sorted(group.items()):
            body.extend(word.encode("utf-8"))
            body.extend(struct.pack("<H", diversity))
        offsets.append(offsets[0] + len(body))
    return struct.pack("<8I", *offsets) + body, sum(map(len, groups.values()))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data, count = build(SOURCE)
    if args.check:
        if OUTPUT.read_bytes() != data:
            raise SystemExit("word-diversity.bin differs from reproducible source output")
    else:
        OUTPUT.write_bytes(data)
    print(f"{count} words, {len(data)} bytes, sha256 {hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    main()
