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
Image files that work under any path.

cv2.imread and cv2.imwrite open files through the C runtime's narrow-character
API on Windows, so a path containing anything outside the ANSI code page fails.
Not loudly: imwrite returns False and writes nothing, imread returns None.
Measured under a folder named `José`: no file, no exception, no warning.

That is not an edge case here. The installed build keeps its data under
%LOCALAPPDATA%, which is inside the user's profile folder, which is named after
the user. Anybody with an accent in their Windows name would have had every
audit picture and every debug frame silently not written.

So encoding happens in memory and Python does the file I/O, which is Unicode
throughout. Same codecs, same parameters, same failure values as cv2's own
functions - None from imread, False from imwrite - so callers need no change
beyond the name.
"""

from __future__ import annotations

from pathlib import Path

import cv2
import numpy as np


def imread(path, flags: int = cv2.IMREAD_COLOR):
    """cv2.imread for any path. None if the file is missing or unreadable."""
    try:
        data = np.fromfile(str(path), dtype=np.uint8)
    except OSError:
        return None
    if not data.size:
        return None
    return cv2.imdecode(data, flags)


def imwrite(path, img, params=None) -> bool:
    """cv2.imwrite for any path. The format comes from the extension."""
    path = Path(path)
    try:
        ok, buf = cv2.imencode(path.suffix or ".png", img, params or [])
    except cv2.error:
        return False
    if not ok:
        return False
    try:
        path.write_bytes(buf.tobytes())
    except OSError:
        return False
    return True
