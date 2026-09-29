#!/usr/bin/env python3
"""Makes the hostile archive fixtures of A-AR-2 and A-AR-3 (P3 8.2), and the 7z fixtures
of T8.

The files next to this script are its output, committed as they are: small archives whose
shapes exploit parser differentials and path joins. The tests list them (T2) and extract
them (T3); they never regenerate them. Run it from anywhere; it writes into its own
directory. It needs Python's standard library and the zstd, xz, bzip2, gzip and bsdtar
tools. It uses no ctypes and writes every byte on purpose: tarfile and zipfile for the
ordinary parts, struct for the crafted ones. bsdtar writes the ordinary 7z archives (no 7z
tool is needed); the crafted ones come from sz() below, a minimal writer of plain 7z
headers.

Fixtures (member names as stored):

  traversal.tar            ../evil, /abs/file, a/../../b, ok/file, ./dot/ok
  nul-name.tar             a pax path with a NUL byte; ok
  symlink-write-through    link -> ../outside, then link/pwned (the CVE-2025-29787 shape),
    .tar and .zip          as a tar and as a zip
  symlink-chmod.tar        d -> ../outside, then a directory member d with mode 0777 (the
                           RUSTSEC-2026-0067 shape)
  pax-size.tar             a pax size of 1024 over a ustar size of 512; the second data
                           block is a valid header of a member "smuggled" that only a
                           parser ignoring the pax size lists (the RUSTSEC-2026-0068 shape)
  hardlinks.tar            target; hard links to /etc/passwd, ../outside, missing, target
  special.tar              a character and a block device, a FIFO, a setuid file, a
                           setgid directory
  overlap.zip              two central directory entries, a and b, for one local entry
  bomb.zip                 10 MiB of zeros deflated under a declared size of 1000
  bomb.tar.zst             a member of 1 GiB of zeros, then "after"
  longname-bomb.tar.zst    a GNU long-name header that declares 64 MiB of name
  large-window.tar.zst     a zstd frame that needs a 256 MiB window
  large-dict.tar.xz        an xz block whose LZMA2 dictionary is 256 MiB
  truncated.tar.{zst,xz,gz,bz2}
                           24 members, cut at 60 % of the compressed stream
  truncated.tar            6 members, cut inside the second member's data
  encrypted.zip            a ZipCrypto entry (its encryption header is random: this one
                           file differs on every run)

7z (bsdtar, which stores each member's ctime and atime too: these differ on every run):

  solid.7z                 one LZMA block of nine members: a directory tree, non-ASCII
                           names, an empty file, a symlink
  traversal.7z             ../evil, /abs/file, a/../../b, ok/file (bsdtar -P -s)

7z (sz(), byte by byte):

  shapes.7z                one block per member (not solid): Copy, LZMA, LZMA2, deflate and
                           bzip2 coders; a directory, an empty file, a symlink, a FIFO, a
                           setuid mode, a read-only file without a Unix mode
  large-dict.7z            an LZMA2 block that declares a 256 MiB dictionary, then an LZMA
                           block that decodes
  encrypted.7z             a block whose coder is AES, then a Copy block
  header-bomb.7z           a compressed header of 16 MiB that declares 16 Mi files
  bad-name.7z              a name with an unpaired UTF-16 surrogate
  interleaved.7z           an empty file listed between the two members of one block
  unsupported.7z           a zstd and a PPMd block, which the reader leaves out, then a Copy
                           block
"""

import io
import lzma
import os
import shutil
import struct
import subprocess
import tarfile
import threading
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
MTIME = 1_700_000_000


def out(name, data):
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data)


def info(name, kind=tarfile.REGTYPE, size=0, mode=0o644, link=""):
    t = tarfile.TarInfo(name)
    t.type = kind
    t.size = size
    t.mode = mode
    t.mtime = MTIME
    t.uid = t.gid = 0
    t.uname = t.gname = ""
    t.linkname = link
    return t


def tar_bytes(members, fmt=tarfile.GNU_FORMAT):
    """members: (TarInfo, data or None)."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=fmt) as tf:
        for t, data in members:
            tf.addfile(t, io.BytesIO(data) if data is not None else None)
    return buf.getvalue()


def pipe(argv, data):
    return subprocess.run(argv, input=data, stdout=subprocess.PIPE, check=True).stdout


def block(data):
    return data + b"\0" * (-len(data) % 512)


def header(t):
    return t.tobuf(tarfile.USTAR_FORMAT, "utf-8", "surrogateescape")


def pax_record(key, value):
    body = b" " + key + b"=" + value + b"\n"
    n = len(body) + 1
    while len(str(n).encode()) + len(body) != n:
        n += 1
    return str(n).encode() + body


def file(name, data, mode=0o644):
    return info(name, size=len(data), mode=mode), data


# -- tar shapes ---------------------------------------------------------------------------

out(
    "traversal.tar",
    tar_bytes(
        [
            file("../evil", b"evil\n"),
            file("/abs/file", b"abs\n"),
            file("a/../../b", b"b\n"),
            file("ok/file", b"ok\n"),
            file("./dot/ok", b"dot\n"),
        ]
    ),
)

nul = info("placeholder", size=3)
nul.pax_headers = {"path": "a\0b"}
out("nul-name.tar", tar_bytes([(nul, b"nul"), file("ok", b"ok\n")], tarfile.PAX_FORMAT))

out(
    "symlink-write-through.tar",
    tar_bytes(
        [
            (info("link", tarfile.SYMTYPE, mode=0o777, link="../outside"), None),
            file("link/pwned", b"written through the link\n"),
            file("fine", b"fine\n"),
        ]
    ),
)

out(
    "symlink-chmod.tar",
    tar_bytes(
        [
            (info("d", tarfile.SYMTYPE, mode=0o777, link="../outside"), None),
            (info("d", tarfile.DIRTYPE, mode=0o777), None),
        ]
    ),
)

# pax-size.tar, byte by byte: pax(size=1024), a (ustar size 512), 1024 bytes of data whose
# second block is the header of "smuggled", then b and the end blocks.
pax_data = pax_record(b"size", b"1024")
pax = info("PaxHeaders/a", tarfile.XHDTYPE, size=len(pax_data))
a = info("a", size=512)
smuggled = info("smuggled", size=0)
b_data = b"after the pax member\n"
b_info = info("b", size=len(b_data))
raw = (
    header(pax)
    + block(pax_data)
    + header(a)
    + b"\0" * 512
    + header(smuggled)
    + header(b_info)
    + block(b_data)
    + b"\0" * 1024
)
out("pax-size.tar", raw)

out(
    "hardlinks.tar",
    tar_bytes(
        [
            file("target", b"the target\n"),
            (info("hl-abs", tarfile.LNKTYPE, link="/etc/passwd"), None),
            (info("hl-up", tarfile.LNKTYPE, link="../outside"), None),
            (info("hl-missing", tarfile.LNKTYPE, link="missing"), None),
            (info("hl-ok", tarfile.LNKTYPE, link="target"), None),
        ]
    ),
)

chr_dev = info("dev-null", tarfile.CHRTYPE, mode=0o666)
chr_dev.devmajor, chr_dev.devminor = 1, 3
blk_dev = info("dev-block", tarfile.BLKTYPE, mode=0o660)
blk_dev.devmajor, blk_dev.devminor = 8, 0
out(
    "special.tar",
    tar_bytes(
        [
            (chr_dev, None),
            (blk_dev, None),
            (info("fifo", tarfile.FIFOTYPE, mode=0o644), None),
            file("setuid", b"#!/bin/sh\n", mode=0o4755),
            (info("setgid-dir", tarfile.DIRTYPE, mode=0o2755), None),
        ]
    ),
)

# -- zip shapes ---------------------------------------------------------------------------


def zinfo(name, mode, method=zipfile.ZIP_STORED):
    z = zipfile.ZipInfo(name, date_time=(2023, 11, 14, 22, 13, 20))
    z.create_system = 3
    z.external_attr = mode << 16
    z.compress_type = method
    return z


buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as z:
    z.writestr(zinfo("link", 0o120777), "../outside")
    z.writestr(zinfo("link/pwned", 0o100644), "written through the link\n")
    z.writestr(zinfo("fine", 0o100644), "fine\n")
out("symlink-write-through.zip", buf.getvalue())


def zip_parts(data):
    """(bytes before the central directory, central records, end record)."""
    eocd = data.rindex(b"PK\x05\x06")
    count, cd_size, cd_off = struct.unpack("<HII", data[eocd + 10 : eocd + 20])
    cd = data[cd_off : cd_off + cd_size]
    records, at = [], 0
    for _ in range(count):
        n, e, c = struct.unpack("<HHH", cd[at + 28 : at + 34])
        size = 46 + n + e + c
        records.append(bytearray(cd[at : at + size]))
        at += size
    return data[:cd_off], records, bytearray(data[eocd:])


def zip_join(body, records, eocd):
    cd = b"".join(records)
    eocd[8:10] = struct.pack("<H", len(records))
    eocd[10:12] = struct.pack("<H", len(records))
    eocd[12:16] = struct.pack("<I", len(cd))
    eocd[16:20] = struct.pack("<I", len(body))
    return body + cd + bytes(eocd)


buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr(zinfo("a", 0o100644, zipfile.ZIP_DEFLATED), b"shared data\n" * 100)
body, recs, eocd = zip_parts(buf.getvalue())
twin = bytearray(recs[0])
twin[46:47] = b"b"
out("overlap.zip", zip_join(body, recs + [twin], eocd))

buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr(zinfo("bomb", 0o100644, zipfile.ZIP_DEFLATED), b"\0" * (10 << 20))
    z.writestr(zinfo("small", 0o100644), b"small\n")
data = bytearray(buf.getvalue())
body, recs, eocd = zip_parts(bytes(data))
body = bytearray(body)
struct.pack_into("<I", body, 22, 1000)  # the local header's uncompressed size
struct.pack_into("<I", recs[0], 24, 1000)  # the central directory's
out("bomb.zip", zip_join(bytes(body), recs, eocd))

enc_dir = os.path.join(HERE, ".enc")
os.makedirs(enc_dir, exist_ok=True)
with open(os.path.join(enc_dir, "secret.txt"), "wb") as f:
    f.write(b"secret data\n")
os.utime(os.path.join(enc_dir, "secret.txt"), (MTIME, MTIME))
subprocess.run(
    [
        "bsdtar",
        "--format",
        "zip",
        "--options",
        "zip:encryption=zipcrypt",
        "--passphrase",
        "fixture",
        "--uid",
        "0",
        "--gid",
        "0",
        "-C",
        enc_dir,
        "-cf",
        os.path.join(HERE, "encrypted.zip"),
        "secret.txt",
    ],
    check=True,
)
os.remove(os.path.join(enc_dir, "secret.txt"))
os.rmdir(enc_dir)

# -- compressed streams -------------------------------------------------------------------

# A member of 1 GiB of zeros, streamed into zstd, then a small member.
zs = subprocess.Popen(["zstd", "-q", "-c"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
big = info("zeros", size=1 << 30)
chunk = b"\0" * (1 << 20)


def feed():
    zs.stdin.write(header(big))
    for _ in range(1 << 10):
        zs.stdin.write(chunk)
    after = b"after the zeros\n"
    zs.stdin.write(header(info("after", size=len(after))) + block(after) + b"\0" * 1024)
    zs.stdin.close()


t = threading.Thread(target=feed)
t.start()
bomb = zs.stdout.read()
t.join()
zs.wait()
out("bomb.tar.zst", bomb)

# A GNU long-name header that declares 64 MiB of name.
long_size = 64 << 20
ln = info("././@LongLink", tarfile.GNUTYPE_LONGNAME, size=long_size)
zs = subprocess.Popen(["zstd", "-q", "-c"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)


def feed_long():
    zs.stdin.write(ln.tobuf(tarfile.GNU_FORMAT))
    for _ in range(long_size >> 20):
        zs.stdin.write(b"a" * (1 << 20))
    zs.stdin.write(header(info("x", size=0)) + b"\0" * 1024)
    zs.stdin.close()


t = threading.Thread(target=feed_long)
t.start()
longbomb = zs.stdout.read()
t.join()
zs.wait()
out("longname-bomb.tar.zst", longbomb)

small = tar_bytes([file("small", b"small\n")])
# From a pipe zstd writes the window it was asked for into the frame header.
out("large-window.tar.zst", pipe(["zstd", "--long=28", "-q", "-c"], small))

# The first block header's LZMA2 dictionary raised to 256 MiB, its CRC32 recomputed.
xz = bytearray(pipe(["xz", "-c"], small))
h_size = (xz[12] + 1) * 4
hdr = xz[12 : 12 + h_size]
at = hdr.index(b"\x21\x01")
hdr[at + 2] = 32
struct.pack_into("<I", hdr, h_size - 4, zlib.crc32(bytes(hdr[: h_size - 4])))
xz[12 : 12 + h_size] = hdr
out("large-dict.tar.xz", bytes(xz))

members = [
    file(f"m{i:02}.txt", "".join(f"member {i} line {k}\n" for k in range(400)).encode())
    for i in range(24)
]
plain = tar_bytes(members)
for ext, argv in [
    # Small blocks, so the blocks before the cut decode: a zstd block and an LZMA2 chunk
    # decode only when complete.
    ("zst", ["zstd", "-q", "-c", "--target-compressed-block-size=1024"]),
    ("xz", ["xz", "-c", "--block-size=16KiB"]),
    ("gz", ["gzip", "-n", "-c"]),
    ("bz2", ["bzip2", "-1", "-c"]),
]:
    whole = pipe(argv, plain)
    out(f"truncated.tar.{ext}", whole[: len(whole) * 6 // 10])
short = tar_bytes(members[:6])
out("truncated.tar", short[: 4 * 512 * 6 + 700])

# -- 7z shapes ----------------------------------------------------------------------------

src7 = os.path.join(HERE, ".7z-src")


def tree(files, links=()):
    """A source tree for bsdtar: {path: bytes or None (a directory)}, (link, target) pairs;
    every mtime is MTIME."""
    shutil.rmtree(src7, ignore_errors=True)
    for path, data in files.items():
        full = os.path.join(src7, path)
        if data is None:
            os.makedirs(full, exist_ok=True)
        else:
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "wb") as f:
                f.write(data)
    for link, target in links:
        os.symlink(target, os.path.join(src7, link))
    for root, dirs, names in os.walk(src7, topdown=False):
        for n in dirs + names:
            os.utime(os.path.join(root, n), (MTIME, MTIME), follow_symlinks=False)


def bsdtar_7z(name, members, extra=()):
    subprocess.run(
        ["bsdtar", "--format", "7zip", *extra, "-C", src7, "-cf", os.path.join(HERE, name)]
        + members,
        check=True,
    )


tree(
    {
        "d": None,
        "d/e": None,
        "d/e/deep.txt": b"deep in the tree\n" * 40,
        "d/f.txt": b"f\n" * 300,
        "\u00fc\u00f1\u00ef": None,
        "\u00fc\u00f1\u00ef/\u00e7a.txt": "\u00e7a va\n".encode() * 50,
        "empty": b"",
        "a.txt": b"member a\n" * 100,
        "b.txt": b"member b\n" * 200,
        "c.txt": bytes(range(256)) * 16,
    },
    [("link", "d/f.txt")],
)
bsdtar_7z("solid.7z", ["d", "\u00fc\u00f1\u00ef", "empty", "a.txt", "b.txt", "c.txt", "link"])

tree({"evil": b"evil\n", "abs": b"abs\n", "b": b"b\n", "ok/file": b"ok\n"})
bsdtar_7z(
    "traversal.7z",
    ["evil", "abs", "b", "ok/file"],
    ["-P", "-s", ",^evil$,../evil,", "-s", ",^abs$,/abs/file,", "-s", ",^b$,a/../../b,"],
)
shutil.rmtree(src7)

SZ_SIG = b"7z\xbc\xaf\x27\x1c"
NT_EPOCH = 116_444_736_000_000_000
ID_COPY, ID_LZMA, ID_LZMA2 = b"\x00", b"\x03\x01\x01", b"\x21"
ID_DEFLATE, ID_BZIP2, ID_AES = b"\x04\x01\x08", b"\x04\x02\x02", b"\x06\xf1\x07\x01"
ID_ZSTD, ID_PPMD = b"\x04\xf7\x11\x01", b"\x03\x04\x01"
UNIX_EXT, ATTR_DIR, ATTR_READONLY, ATTR_ARCHIVE = 0x8000, 0x10, 0x01, 0x20


def num7(n):
    """A 7z NUMBER: the leading one bits of the first byte count the bytes after it."""
    for i in range(8):
        if n < 1 << (7 * (i + 1)):
            first = (0xFF00 >> i) & 0xFF | n >> (8 * i)
            return bytes([first]) + (n & ((1 << (8 * i)) - 1)).to_bytes(i, "little")
    return b"\xff" + n.to_bytes(8, "little")


def bits7(flags):
    out = bytearray((len(flags) + 7) // 8)
    for i, f in enumerate(flags):
        if f:
            out[i // 8] |= 0x80 >> (i % 8)
    return bytes(out)


def ux(mode):
    """Attributes with a Unix mode in the high half (the 0x8000 extension)."""
    return mode << 16 | UNIX_EXT | (ATTR_DIR if mode & 0o170000 == 0o040000 else ATTR_ARCHIVE)


def lzma1(data):
    f = {"id": lzma.FILTER_LZMA1, "dict_size": 1 << 16, "lc": 3, "lp": 0, "pb": 2}
    props = bytes([(2 * 5 + 0) * 9 + 3]) + struct.pack("<I", 1 << 16)
    return ID_LZMA, props, lzma.compress(data, format=lzma.FORMAT_RAW, filters=[f])


def lzma2(data, dict_prop=16):
    """LZMA2 data made with a 64 KiB dictionary; the properties declare `dict_prop`."""
    f = {"id": lzma.FILTER_LZMA2, "dict_size": 1 << 16}
    return ID_LZMA2, bytes([dict_prop]), lzma.compress(data, format=lzma.FORMAT_RAW, filters=[f])


def deflate(data):
    c = zlib.compressobj(9, zlib.DEFLATED, -15)
    return ID_DEFLATE, b"", c.compress(data) + c.flush()


def bzip2(data):
    import bz2

    return ID_BZIP2, b"", bz2.compress(data)


def copy(data):
    return ID_COPY, b"", data


def utf16(name):
    """A name as UTF-16LE: a str, or a list of code units (an invalid name)."""
    if isinstance(name, str):
        return name.encode("utf-16-le")
    return struct.pack(f"<{len(name)}H", *name)


def sz(entries, blocks):
    """A 7z with a plain header, written byte by byte.

    entries: (name, attributes, data) in file order; data None is a directory, b"" an empty
    file. blocks: (coder id, properties, packed bytes, members) in pack order, `members` the
    number of streams in the block; the entries with data fill the blocks' streams in file
    order.
    """
    streamed = [e[2] for e in entries if e[2]]
    packed = b"".join(b[2] for b in blocks)
    h = bytearray(b"\x01\x04")
    h += b"\x06" + num7(0) + num7(len(blocks)) + b"\x09"
    h += b"".join(num7(len(b[2])) for b in blocks) + b"\x00"
    h += b"\x07\x0b" + num7(len(blocks)) + b"\x00"
    at, sizes, unpack = 0, [], []
    for cid, props, _, n in blocks:
        h += bytes([1, len(cid) | (0x20 if props else 0)]) + cid
        if props:
            h += num7(len(props)) + props
        mine = streamed[at : at + n]
        at += n
        sizes += [len(d) for d in mine[:-1]]
        unpack.append(sum(len(d) for d in mine))
    h += b"\x0c" + b"".join(num7(u) for u in unpack) + b"\x00"
    h += b"\x08\x0d" + b"".join(num7(b[3]) for b in blocks)
    h += b"\x09" + b"".join(num7(s) for s in sizes)
    h += b"\x0a\x01" + b"".join(struct.pack("<I", zlib.crc32(d)) for d in streamed)
    h += b"\x00\x00"
    n = len(entries)
    h += b"\x05" + num7(n)
    empty = [not e[2] for e in entries]
    if any(empty):
        v = bits7(empty)
        h += b"\x0e" + num7(len(v)) + v
        v = bits7([e[2] == b"" for e in entries if not e[2]])
        h += b"\x0f" + num7(len(v)) + v
    names = b"".join(utf16(e[0]) + b"\0\0" for e in entries)
    h += b"\x11" + num7(len(names) + 1) + b"\x00" + names
    t = struct.pack("<Q", NT_EPOCH + MTIME * 10_000_000)
    h += b"\x14" + num7(2 + 8 * n) + b"\x01\x00" + t * n
    h += b"\x15" + num7(2 + 4 * n) + b"\x01\x00"
    h += b"".join(struct.pack("<I", e[1]) for e in entries)
    h += b"\x00\x00"
    return sz_join(packed, bytes(h))


def sz_join(packed, header):
    start = struct.pack("<QQI", len(packed), len(header), zlib.crc32(header))
    return SZ_SIG + b"\x00\x04" + struct.pack("<I", zlib.crc32(start)) + start + packed + header


def block(coder, *datas):
    cid, props, packed = coder(b"".join(datas))
    return cid, props, packed, len(datas)


shape_data = {
    "deflated": b"deflated member\n" * 30,
    "bzip2ed": b"bzip2 member\n" * 30,
    "stored": b"stored member\n",
    "lzma2ed": b"lzma2 member\n" * 30,
    "lzmaed": b"lzma member\n" * 30,
}
out(
    "shapes.7z",
    sz(
        [
            ("dir", ux(0o040750), None),
            ("dir/deflated", ux(0o100640), shape_data["deflated"]),
            ("dir/bzip2ed", ux(0o104755), shape_data["bzip2ed"]),
            ("stored", ATTR_READONLY | ATTR_ARCHIVE, shape_data["stored"]),
            ("lzma2ed", ux(0o100644), shape_data["lzma2ed"]),
            ("lzmaed", ux(0o100600), shape_data["lzmaed"]),
            ("link", ux(0o120777), b"dir/deflated"),
            ("empty", ux(0o100600), b""),
            ("fifo", ux(0o010644), b""),
        ],
        [
            block(deflate, shape_data["deflated"]),
            block(bzip2, shape_data["bzip2ed"]),
            block(copy, shape_data["stored"]),
            block(lzma2, shape_data["lzma2ed"]),
            block(lzma1, shape_data["lzmaed"]),
            block(copy, b"dir/deflated"),
        ],
    ),
)

# The first block's LZMA2 properties declare a 256 MiB dictionary (property 32).
cid, props, packed = lzma2(b"small\n")
out(
    "large-dict.7z",
    sz(
        [("small", ux(0o100644), b"small\n"), ("other", ux(0o100644), b"other\n")],
        [(cid, bytes([32]), packed, 1), block(lzma1, b"other\n")],
    ),
)

# An AES coder over bytes that are not decoded: the member is listed and refused.
out(
    "encrypted.7z",
    sz(
        [("secret.txt", ux(0o100644), b"secret data\n"), ("plain.txt", ux(0o100644), b"plain\n")],
        [(ID_AES, bytes([0x13, 0x00]), b"\x5a" * 16, 1), block(copy, b"plain\n")],
    ),
)

# A header that declares 16 Mi files, padded to 16 MiB with a dummy property, compressed
# with LZMA: without the walk's bound the reader would build 16 Mi entries.
files = 16 << 20
plain = b"\x01\x05" + num7(files) + b"\x19" + num7(16 << 20) + bytes(16 << 20) + b"\x00\x00"
cid, props, packed = lzma1(plain)
enc = b"\x17\x06" + num7(0) + b"\x01\x09" + num7(len(packed)) + b"\x00"
enc += b"\x07\x0b\x01\x00\x01" + bytes([len(cid) | 0x20]) + cid + num7(len(props)) + props
enc += b"\x0c" + num7(len(plain)) + b"\x0a\x01" + struct.pack("<I", zlib.crc32(plain))
enc += b"\x00\x00"
out("header-bomb.7z", sz_join(packed, enc))

out(
    "bad-name.7z",
    sz(
        [([0x62, 0xD800, 0x61], ux(0o100644), b"bad\n"), ("ok", ux(0o100644), b"ok\n")],
        [block(copy, b"bad\n", b"ok\n")],
    ),
)

out(
    "interleaved.7z",
    sz(
        [
            ("first", ux(0o100644), b"first member\n"),
            ("between", ux(0o100644), b""),
            ("second", ux(0o100644), b"second member\n"),
        ],
        [block(lzma1, b"first member\n", b"second member\n")],
    ),
)

# Coders the reader leaves out: zstd (its crate line would add a second libzstd) and PPMd.
# Their data is never decoded; the Copy block after them is.
out(
    "unsupported.7z",
    sz(
        [
            ("zstd", ux(0o100644), b"zstd member\n"),
            ("ppmd", ux(0o100644), b"ppmd member\n"),
            ("plain", ux(0o100644), b"plain\n"),
        ],
        [
            (ID_ZSTD, b"", b"\x28\xb5\x2f\xfd" + bytes(8), 1),
            (ID_PPMD, bytes([6, 0, 0, 0x10, 0]), bytes(12), 1),
            block(copy, b"plain\n"),
        ],
    ),
)
