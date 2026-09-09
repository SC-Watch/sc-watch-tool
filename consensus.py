# sc-watch - reads player handles off the Star Citizen HUD.
# Copyright (C) 2026 SC-Watch
#
# This program is free software: you can redistribute it and/or modify it
# under the terms of the GNU General Public License as published by the Free
# Software Foundation, either version 3 of the License, or (at your option)
# any later version.
#
# This program is distributed in the hope that it will be useful, but WITHOUT
# ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
# FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
# more details. You should have received a copy of the GNU General Public
# License along with this program. If not, see <https://www.gnu.org/licenses/>.

"""
Turn a burst of noisy per-frame reads into one confident identification.

Per-frame OCR runs 92-100% accurate, and every error observed so far is a
single occurrence against many correct reads:

    SP-9695-PW    x37   vs  SP-969        x2
    SAMURAIBIKER  x33   vs  SAMURAIBIER   x1,  SAMURAI  x1
    JR-8405-HU    x5    vs  JR-B405-HU    x1

So the fix is not better OCR, it is reading the same label several times and
letting the readings outvote each other. Two pieces:

  CLUSTER   Group readings that refer to the same physical contact. Range and
            screen position barely move over a two-second burst, and the names
            are near-identical, so all three agree on the grouping.

  VOTE      Take the modal reading per cluster. Ties break toward the longer
            string, because the characteristic OCR failure is dropping
            characters at the edges, never inventing them.

The same edit distance also matches a read against the database later, which
is where the confusion weighting earns its keep: a name misread by one
confusable character should still match its database entry, while a genuinely
different handle should not.
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field

# Character pairs OCR actually confuses, at reduced substitution cost.
# Seeded from observed errors (8->B, D->0, K dropped) plus the usual suspects
# for small sans-serif type.
_CONFUSABLE = [
    # '_' belongs with the tall thin glyphs: MALPHAS_E14 read as MALPHASIE14
    # in a real session, and underscores are common in handles.
    "0OD", "0Q", "1IL_", "1T", "5S", "8B", "6G", "2Z", "9G", "9P",
    "CG", "EF", "MN", "UV", "VY", "KX", "PR", "7T",
]

_CONFUSION_COST = 0.25
_NORMAL_COST = 1.0


def _build_costs():
    costs = {}
    for group in _CONFUSABLE:
        for a in group:
            for b in group:
                if a != b:
                    costs[(a, b)] = _CONFUSION_COST
    return costs


_COSTS = _build_costs()


def sub_cost(a: str, b: str) -> float:
    if a == b:
        return 0.0
    return _COSTS.get((a.upper(), b.upper()), _NORMAL_COST)


def distance(a: str, b: str) -> float:
    """Weighted Levenshtein. Substituting a confusable pair costs a quarter."""
    a, b = a.upper(), b.upper()
    if not a:
        return float(len(b))
    if not b:
        return float(len(a))

    prev = [float(j) for j in range(len(b) + 1)]
    for i, ca in enumerate(a, 1):
        cur = [float(i)]
        for j, cb in enumerate(b, 1):
            cur.append(min(
                prev[j] + 1.0,            # deletion
                cur[j - 1] + 1.0,         # insertion
                prev[j - 1] + sub_cost(ca, cb),
            ))
        prev = cur
    return prev[-1]


def similarity(a: str, b: str) -> float:
    """0..1, where 1 is identical."""
    if not a and not b:
        return 1.0
    return 1.0 - distance(a, b) / max(len(a), len(b), 1)


@dataclass
class Reading:
    name: str
    range_km: float
    cx: float = 0.0
    cy: float = 0.0


@dataclass
class Resolved:
    name: str
    range_km: float
    votes: int
    total: int
    variants: list = field(default_factory=list)

    @property
    def confidence(self) -> float:
        return self.votes / max(self.total, 1)


NAME_SIMILARITY = 0.62   # readings this alike are the same contact
RANGE_TOLERANCE = 0.6    # km; a contact barely moves during a burst
RANGE_TOLERANCE_FRAC = 0.15  # ...but tolerance has to scale with distance
MIN_PREFIX = 4           # shortest truncation we'll trust as the same contact


def range_tolerance(km: float) -> float:
    """How far apart two readings can be and still be one contact.

    A flat 0.6km works close in but breaks at range, because a single-digit
    OCR slip is proportionally larger there: 28.4km misread as 20.4km is 8km
    apart, so the two readings cluster separately and one contact becomes two,
    each with a single vote. Real movement scales with distance too - a ship
    4km out barely changes range in a second, one at 30km can shift much more.
    """
    return max(RANGE_TOLERANCE, abs(km) * RANGE_TOLERANCE_FRAC)


def same_contact(a: str, b: str) -> bool:
    """Two readings of one label.

    Plain similarity misses truncations: 'SP-969' against 'SP-9695-PW' scores
    0.60 because four of ten characters are gone, yet it is obviously the same
    contact. Every truncation observed - SP-969, SAMURAI, DAMADZK, JR-840 - is
    a PREFIX of the full string, because the recogniser gives up at the end of
    a line rather than mangling the middle. So treat a prefix as a match
    outright instead of loosening the threshold for everything.
    """
    a, b = a.upper(), b.upper()
    if similarity(a, b) >= NAME_SIMILARITY:
        return True
    short, long = (a, b) if len(a) <= len(b) else (b, a)
    return len(short) >= MIN_PREFIX and long.startswith(short)


def cluster(readings: list[Reading]) -> list[list[Reading]]:
    """Greedy agglomeration. Bursts hold a handful of contacts, so the
    quadratic cost is irrelevant and the simplicity is worth keeping."""
    clusters: list[list[Reading]] = []
    for r in readings:
        if not r.name:
            continue
        placed = False
        for c in clusters:
            # Compare against the cluster's current best guess rather than an
            # arbitrary member, so one bad early read can't anchor the group.
            rep = Counter(x.name for x in c).most_common(1)[0][0]
            rep_range = sorted(x.range_km for x in c)[len(c) // 2]
            if not same_contact(r.name, rep):
                continue
            # Range is a secondary guard - its job is to stop two different
            # ships that happen to read alike from merging. When the handles
            # match EXACTLY that risk is negligible (two players with identical
            # names in one burst), so a range disagreement means the range was
            # misread, not that these are two ships. That matters most at
            # distance, where a single tens-digit slip puts 28.4km and 20.4km
            # 8km apart and splits one contact into two single-vote phantoms.
            exact = r.name.upper() == rep.upper()
            tol = range_tolerance(max(r.range_km, rep_range))
            if exact or abs(r.range_km - rep_range) <= tol:
                c.append(r)
                placed = True
                break
        if not placed:
            clusters.append([r])
    return clusters


def vote(group: list[Reading]) -> Resolved:
    counts = Counter(r.name for r in group)
    top = counts.most_common()
    best_n = top[0][1]
    # Ties go to the longer string: OCR drops characters, it doesn't add them.
    winner = max((n for n, c in top if c == best_n), key=len)
    ranges = sorted(r.range_km for r in group)
    return Resolved(
        name=winner,
        range_km=ranges[len(ranges) // 2],
        votes=counts[winner],
        total=len(group),
        variants=[(n, c) for n, c in top if n != winner],
    )


def resolve(readings: list[Reading], min_votes: int = 2) -> list[Resolved]:
    """Cluster then vote. Contacts seen only once are reported but flagged by
    their vote count rather than silently dropped."""
    out = [vote(g) for g in cluster(readings)]
    out.sort(key=lambda r: (-r.votes, r.range_km))
    return [r for r in out if r.votes >= min_votes or r.total >= min_votes]
