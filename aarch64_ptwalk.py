#!/usr/bin/env python3
# aarch64_ptwalk.py
#
# AArch64 stage-1 page table walker for 4KB granule.
# - Works over a physical memory dump (dump.bin) with a known dump base PA.
# - Uses TCR_EL1 (T0SZ/T1SZ) to:
#     * choose TTBR0 vs TTBR1 for the given VA
#     * compute the start level (may be L0..L3)
# - Walks like the CPU: table desc -> next level, leaf (block/page) -> PA
# - Prints readable flags and effective permissions (EL1/EL0 R/W/X),
#   including hierarchical restrictions from table descriptors:
#     APTable, PXNTable, UXNTable
#
# Assumptions:
# - 4KB granule, stage-1 translation, up to 48-bit VA typical.
# - Does not model SCTLR_EL1.WXN, PAN, PXN/UXN interactions with EL2, etc.
#
# Usage:
#   Interactive:
#     python3 aarch64_ptwalk.py --mem-base 0x40000000 --trace
#
#   Non-interactive:
#     python3 aarch64_ptwalk.py --mem-base 0x40000000 --tcr 0xb5103510 \
#       --ttbr0 0x40000000 --ttbr1 0x0 --va 0xffff000000203784
#
# Dump creation (QEMU monitor):
#   (qemu) stop
#   (qemu) pmemsave 0x40000000 0x20000000 dump.bin
#   (qemu) cont

from __future__ import annotations

import argparse
import struct
from dataclasses import dataclass
from typing import Optional, List, Tuple, Any


U64_MASK = (1 << 64) - 1

# Address field masks for stage-1 descriptors (4KB granule)
TABLE_NEXT_MASK = 0x0000_FFFF_FFFF_F000

# Leaf output address masks (4KB granule, 4-level)
L1_BLOCK_MASK = 0x0000_FFFF_C000_0000  # 1GB block
L2_BLOCK_MASK = 0x0000_FFFF_FFE0_0000  # 2MB block
L3_PAGE_MASK  = 0x0000_FFFF_FFFF_F000  # 4KB page


# ========= bit helpers =========

def parse_int(s: str) -> int:
    s = s.strip().lower()
    return int(s, 16) if s.startswith("0x") else int(s, 10)

def bit(x: int, n: int) -> int:
    return (x >> n) & 1

def bits(x: int, hi: int, lo: int) -> int:
    return (x >> lo) & ((1 << (hi - lo + 1)) - 1)


# ========= pretty decode helpers =========

def sh_str(v: int) -> str:
    # SH[9:8]
    return {0b00: "NSH", 0b10: "OSH", 0b11: "ISH"}.get(v, f"RES({v:02b})")

def ap_leaf_str(ap: int) -> str:
    # AP[7:6]
    return {
        0b00: "EL1 RW, EL0 --",
        0b01: "EL1 RW, EL0 RW",
        0b10: "EL1 RO, EL0 --",
        0b11: "EL1 RO, EL0 RO",
    }.get(ap, f"AP({ap:02b})")


# ========= dump reader =========

class DumpReader:
    def __init__(self, path: str, dump_base_pa: int):
        self.dump_base_pa = dump_base_pa
        with open(path, "rb") as f:
            self.data = f.read()

    def read_u64(self, pa: int) -> int:
        off = pa - self.dump_base_pa
        if off < 0 or off + 8 > len(self.data):
            raise ValueError(
                f"PA 0x{pa:x} out of dump (base=0x{self.dump_base_pa:x}, size=0x{len(self.data):x})"
            )
        return struct.unpack_from("<Q", self.data, off)[0] & U64_MASK


# ========= TCR handling (TTBR selection + start level) =========

@dataclass
class TcrCfg:
    t0sz: int
    t1sz: int

    @staticmethod
    def from_tcr(tcr: int) -> "TcrCfg":
        return TcrCfg(
            t0sz=bits(tcr, 5, 0),
            t1sz=bits(tcr, 21, 16),
        )

def va_bits_from_tsz(tsz: int) -> int:
    return 64 - tsz

def va_region_by_tcr(va_u64: int, tcr: TcrCfg) -> str:
    """
    Return TTBR0 / TTBR1 / INVALID according to T0SZ/T1SZ.
    We operate on unsigned VA.
    """
    low_size = 1 << (64 - tcr.t0sz)
    high_base = (1 << 64) - (1 << (64 - tcr.t1sz))

    if va_u64 < low_size:
        return "TTBR0"
    if va_u64 >= high_base:
        return "TTBR1"
    return "INVALID"

def start_level_for_4k(va_bits: int) -> int:
    """
    For 4KB granule: 12-bit offset + N*9-bit indices.
    levels = ceil((va_bits - 12)/9), clamped to [1..4]
    start_level = 4 - levels (0..3)
    """
    idx_bits = max(0, va_bits - 12)
    levels = (idx_bits + 8) // 9  # ceil
    if levels < 1:
        levels = 1
    if levels > 4:
        levels = 4
    return 4 - levels


# ========= VA breakdown (4KB) =========

def lvl_index_4k(va_u64: int, lvl: int) -> int:
    # absolute L0..L3
    shift = {0: 39, 1: 30, 2: 21, 3: 12}[lvl]
    return (va_u64 >> shift) & 0x1FF

def page_off_4k(va_u64: int) -> int:
    return va_u64 & 0xFFF


# ========= descriptor classify / decode =========

def classify(level: int, desc: int) -> str:
    t = bits(desc, 1, 0)
    if t == 0b00:
        return "INVALID"
    if t == 0b11:
        return "PAGE" if level == 3 else "TABLE"
    if t == 0b01:
        return "BLOCK" if level in (1, 2) else "INVALID"  # L3 block invalid for 4KB granule
    return "INVALID"  # 0b10 reserved in stage-1

def decode_table(desc: int) -> dict:
    # Table descriptor (L0..L2, type=0b11)
    return {
        "NSTable": bit(desc, 63),
        "APTable": bits(desc, 62, 61),  # [1]=force RO, [0]=no EL0 access (hierarchical)
        "UXNTable": bit(desc, 60),
        "PXNTable": bit(desc, 59),
        "Next": desc & TABLE_NEXT_MASK,
    }

def decode_leaf(desc: int) -> dict:
    # Block/Page descriptor
    return {
        "AttrIndx": bits(desc, 4, 2),
        "NS": bit(desc, 5),
        "AP": bits(desc, 7, 6),
        "SH": bits(desc, 9, 8),
        "AF": bit(desc, 10),
        "nG": bit(desc, 11),
        "DBM": bit(desc, 51),
        "Contiguous": bit(desc, 52),
        "PXN": bit(desc, 53),
        "UXN": bit(desc, 54),
    }


# ========= hierarchical permissions =========

@dataclass
class Hier:
    force_ro: bool = False
    forbid_el0: bool = False
    force_pxn: bool = False  # affects EL1 exec
    force_uxn: bool = False  # affects EL0 exec

    def apply_table(self, t: dict) -> None:
        apt = t["APTable"]
        self.force_ro |= bool((apt >> 1) & 1)
        self.forbid_el0 |= bool(apt & 1)
        self.force_pxn |= bool(t["PXNTable"])
        self.force_uxn |= bool(t["UXNTable"])

def perms_from_leaf(h: Hier, leaf: dict) -> dict:
    ap = leaf["AP"]
    leaf_el1_ro = ap in (0b10, 0b11)
    leaf_el0_allowed = ap in (0b01, 0b11)

    el1_r = True
    el1_w = (not h.force_ro) and (not leaf_el1_ro)

    el0_allowed = leaf_el0_allowed and (not h.forbid_el0)
    el0_r = el0_allowed
    # writable for EL0 only if EL0 RW AND not forced RO etc.
    el0_w = el0_allowed and el1_w and (ap == 0b01)

    el1_x = not (h.force_pxn or bool(leaf["PXN"]))
    el0_x = el0_allowed and (not (h.force_uxn or bool(leaf["UXN"])))

    return {
        "EL1": {"R": el1_r, "W": el1_w, "X": el1_x},
        "EL0": {"R": el0_r, "W": el0_w, "X": el0_x},
    }


# ========= walk + trace structures =========

@dataclass
class Step:
    lvl: int
    kind: str            # INFO / TABLE / BLOCK / PAGE / INVALID
    table_pa: int = 0
    idx: int = 0
    desc_pa: int = 0
    desc: int = 0
    flags: List[str] = None
    next_pa: Optional[int] = None
    out_pa: Optional[int] = None
    offset: Optional[int] = None

def fmt_bool(name: str, v: int) -> str:
    return f"{name}={v}"

def walk(reader: DumpReader, tcr_val: int, ttbr0_pa: int, ttbr1_pa: int, va: int) -> Tuple[List[Step], Optional[int], Optional[dict], str]:
    tcr = TcrCfg.from_tcr(tcr_val)
    va_u64 = va & U64_MASK

    region = va_region_by_tcr(va_u64, tcr)
    if region == "INVALID":
        return [
            Step(
                lvl=0,
                kind="INFO",
                flags=[
                    f"T0SZ={tcr.t0sz}, T1SZ={tcr.t1sz}",
                    "VA is not in TTBR0/TTBR1 ranges -> FAULT",
                ],
            )
        ], None, None, "FAULT"

    tsz = tcr.t0sz if region == "TTBR0" else tcr.t1sz
    va_bits = va_bits_from_tsz(tsz)
    start_lvl = start_level_for_4k(va_bits)

    root_pa = (ttbr0_pa if region == "TTBR0" else ttbr1_pa) & TABLE_NEXT_MASK

    h = Hier()
    steps: List[Step] = [
        Step(
            lvl=start_lvl,
            kind="INFO",
            flags=[
                f"Region={region}",
                f"VA_bits={va_bits} (TSZ={tsz})",
                f"StartLevel=L{start_lvl}",
                f"RootPA=0x{root_pa:x}",
            ],
        )
    ]

    table_pa = root_pa
    for lvl in range(start_lvl, 4):
        idx = lvl_index_4k(va_u64, lvl)
        desc_pa = table_pa + idx * 8
        desc = reader.read_u64(desc_pa)
        kind = classify(lvl, desc)

        st = Step(lvl=lvl, kind=kind, table_pa=table_pa, idx=idx, desc_pa=desc_pa, desc=desc, flags=[])

        if kind == "INVALID":
            t = bits(desc, 1, 0)
            why: List[str] = ["FAULT"]
            if t == 0b10:
                why.append("reserved descriptor type (0b10)")
            elif lvl == 3 and t == 0b01:
                why.append("L3 block is invalid for 4KB granule")
            else:
                why.append(f"invalid descriptor type={t:02b}")
            st.flags = why
            steps.append(st)
            return steps, None, None, "FAULT"

        if kind == "TABLE":
            tinfo = decode_table(desc)
            h.apply_table(tinfo)
            st.next_pa = tinfo["Next"]
            st.flags = [
                "Valid",
                fmt_bool("NSTable", tinfo["NSTable"]),
                f"APTable={tinfo['APTable']:02b} (A1={'forceRO' if (tinfo['APTable'] & 2) else '-'}, A0={'noEL0' if (tinfo['APTable'] & 1) else '-'})",
                fmt_bool("PXNTable", tinfo["PXNTable"]),
                fmt_bool("UXNTable", tinfo["UXNTable"]),
            ]
            if (st.next_pa & 0xFFF) != 0:
                st.flags.append("WARN: next table addr not 4K-aligned?")
            steps.append(st)
            table_pa = st.next_pa
            continue

        leaf = decode_leaf(desc)
        st.flags = [
            "Valid",
            f"AttrIndx={leaf['AttrIndx']}",
            fmt_bool("NS", leaf["NS"]),
            f"AP={leaf['AP']:02b} ({ap_leaf_str(leaf['AP'])})",
            f"SH={leaf['SH']:02b} ({sh_str(leaf['SH'])})",
            fmt_bool("AF", leaf["AF"]),
            fmt_bool("nG", leaf["nG"]),
            fmt_bool("DBM", leaf["DBM"]),
            fmt_bool("Contig", leaf["Contiguous"]),
            fmt_bool("PXN", leaf["PXN"]),
            fmt_bool("UXN", leaf["UXN"]),
        ]

        if kind == "BLOCK":
            # Determine block size by level
            if lvl == 1:
                base = desc & L1_BLOCK_MASK
                off = va_u64 & ((1 << 30) - 1)
                st.flags.insert(0, "BlockSize=1GB")
            else:  # lvl == 2
                base = desc & L2_BLOCK_MASK
                off = va_u64 & ((1 << 21) - 1)
                st.flags.insert(0, "BlockSize=2MB")
            st.out_pa = (base + off) & U64_MASK
            st.offset = off
            steps.append(st)
            perms = perms_from_leaf(h, leaf)
            return steps, st.out_pa, perms, "VALID"

        if kind == "PAGE":
            base = desc & L3_PAGE_MASK
            off = page_off_4k(va_u64)
            st.flags.insert(0, "PageSize=4KB")
            st.out_pa = (base + off) & U64_MASK
            st.offset = off
            steps.append(st)
            perms = perms_from_leaf(h, leaf)
            return steps, st.out_pa, perms, "VALID"

        st.kind = "INVALID"
        st.flags = ["FAULT", "unreachable descriptor case"]
        steps.append(st)
        return steps, None, None, "FAULT"

    return steps, None, None, "FAULT"


# ========= printing =========

def print_log(steps: List[Step], status: str, out_pa: Optional[int], perms: Optional[dict]) -> None:
    for st in steps:
        if st.kind == "INFO":
            print("Config: " + ", ".join(st.flags) + "\n")
            continue

        if st.kind == "TABLE":
            print(
                f"L{st.lvl} at 0x{st.table_pa:x} [idx=0x{st.idx:x}] "
                f"desc@0x{st.desc_pa:x}=0x{st.desc:016x}\n"
                f"  is Table ({', '.join(st.flags)}). Points to L{st.lvl+1} at 0x{st.next_pa:x}\n"
            )
            continue

        if st.kind in ("BLOCK", "PAGE"):
            leaf_name = "Block" if st.kind == "BLOCK" else "Page"
            print(
                f"L{st.lvl} at 0x{st.table_pa:x} [idx=0x{st.idx:x}] "
                f"desc@0x{st.desc_pa:x}=0x{st.desc:016x}\n"
                f"  is {leaf_name} ({', '.join(st.flags)}). Offset 0x{st.offset:x}\n"
            )
            continue

        # INVALID / fault
        print(
            f"L{st.lvl} at 0x{st.table_pa:x} [idx=0x{st.idx:x}] "
            f"desc@0x{st.desc_pa:x}=0x{st.desc:016x}\n"
            f"  is INVALID ({', '.join(st.flags)})\n"
        )

    if status != "VALID":
        print("Translation is FAULT.")
        return

    print(f"Output PA: 0x{out_pa:x}")
    print("Translation is VALID.")

    def rwx(p: dict) -> str:
        return f"readable={'Y' if p['R'] else 'N'}, writable={'Y' if p['W'] else 'N'}, executable={'Y' if p['X'] else 'N'}"

    print(f"  EL1: {rwx(perms['EL1'])}")
    print(f"  EL0: {rwx(perms['EL0'])}")

def perms_compact(perms: dict) -> str:
    def fmt(p: dict) -> str:
        return "".join([("R" if p["R"] else "-"), ("W" if p["W"] else "-"), ("X" if p["X"] else "-")])
    return f"EL1:{fmt(perms['EL1'])} EL0:{fmt(perms['EL0'])}"

@dataclass
class VmapLeaf:
    va_start: int
    va_end: int
    pa_start: int
    pa_end: int
    flags_key: Tuple[Any, ...]  # stable grouping key
    flags_str: str             # printable

@dataclass
class VmapRegion:
    va_start: int
    va_end: int
    kind: str  # unmapped | identity | mapped
    pa_start: Optional[int] = None
    pa_end: Optional[int] = None
    flags_str: str = ""

def level_span_bytes(lvl: int) -> int:
    # For 4KB granule: entry sizes per level
    return 1 << {0: 39, 1: 30, 2: 21, 3: 12}[lvl]

def leaf_pa_base(desc: int, lvl: int) -> int:
    if lvl == 1:
        return desc & L1_BLOCK_MASK
    if lvl == 2:
        return desc & L2_BLOCK_MASK
    if lvl == 3:
        return desc & L3_PAGE_MASK
    raise ValueError(f"Leaf at unsupported level L{lvl}")

def scan_page_tables(
    reader: DumpReader,
    root_pa: int,
    start_lvl: int,
    region_start_va: int,
) -> Tuple[List[VmapLeaf], List[str]]:
    """
    Traverse the translation tables reachable from root_pa and collect all leaf mappings
    as VA ranges with their PA bases and decoded flags (plus effective perms).
    """
    leaves: List[VmapLeaf] = []
    errors: List[str] = []

    def rec(table_pa: int, lvl: int, va_base: int, h_in: Hier) -> bool:
        # returns True if subtree contains at least one leaf
        span = level_span_bytes(lvl)
        any_leaf = False

        for idx in range(512):
            entry_va = (va_base + idx * span) & U64_MASK
            desc_pa = table_pa + idx * 8
            try:
                desc = reader.read_u64(desc_pa)
            except ValueError as e:
                errors.append(f"read_u64 failed at desc@0x{desc_pa:x}: {e}")
                continue

            kind = classify(lvl, desc)
            if kind == "INVALID":
                continue

            if kind == "TABLE":
                tinfo = decode_table(desc)
                h_next = Hier(
                    force_ro=h_in.force_ro,
                    forbid_el0=h_in.forbid_el0,
                    force_pxn=h_in.force_pxn,
                    force_uxn=h_in.force_uxn,
                )
                h_next.apply_table(tinfo)
                next_pa = tinfo["Next"]
                try:
                    child_has = rec(next_pa, lvl + 1, entry_va, h_next)
                except RecursionError:
                    errors.append(f"RecursionError at L{lvl} table 0x{table_pa:x} -> 0x{next_pa:x}")
                    child_has = False
                any_leaf |= child_has
                continue

            # Leaf: BLOCK (L1/L2) or PAGE (L3)
            leaf = decode_leaf(desc)
            perms = perms_from_leaf(h_in, leaf)
            pa_base = leaf_pa_base(desc, lvl)
            va_start = entry_va
            va_end = entry_va + span
            pa_start = pa_base
            pa_end = pa_base + span

            flags_key = (
                leaf["AttrIndx"],
                leaf["NS"],
                leaf["AP"],
                leaf["SH"],
                leaf["AF"],
                leaf["nG"],
                leaf["DBM"],
                leaf["Contiguous"],
                leaf["PXN"],
                leaf["UXN"],
                # effective perms (depends on hierarchy)
                perms_compact(perms),
            )
            flags_str = (
                f"AttrIndx={leaf['AttrIndx']} "
                f"NS={leaf['NS']} "
                f"AP={leaf['AP']:02b} ({ap_leaf_str(leaf['AP'])}) "
                f"SH={leaf['SH']:02b} ({sh_str(leaf['SH'])}) "
                f"AF={leaf['AF']} "
                f"nG={leaf['nG']} "
                f"DBM={leaf['DBM']} "
                f"Contig={leaf['Contiguous']} "
                f"PXN={leaf['PXN']} "
                f"UXN={leaf['UXN']} "
                f"{perms_compact(perms)}"
            )

            leaves.append(
                VmapLeaf(
                    va_start=va_start,
                    va_end=va_end,
                    pa_start=pa_start,
                    pa_end=pa_end,
                    flags_key=flags_key,
                    flags_str=flags_str,
                )
            )
            any_leaf = True

        return any_leaf

    rec(root_pa & TABLE_NEXT_MASK, start_lvl, region_start_va & U64_MASK, Hier())
    leaves.sort(key=lambda x: x.va_start)
    return leaves, errors

def group_regions(leaves: List[VmapLeaf], region_start: int) -> List[VmapRegion]:
    """
    Merge sequential leaf ranges with identical flags and linear VA->PA mapping.
    Then insert unmapped gaps between the mapped regions. Report starts at region_start
    and ends at the end of the last mapped region (to avoid printing enormous empty tails).
    """
    if not leaves:
        return []

    merged: List[VmapLeaf] = []
    for leaf in leaves:
        if not merged:
            merged.append(leaf)
            continue

        prev = merged[-1]
        va_contig = prev.va_end == leaf.va_start
        pa_contig = prev.pa_end == leaf.pa_start
        if va_contig and pa_contig and prev.flags_key == leaf.flags_key:
            merged[-1] = VmapLeaf(
                va_start=prev.va_start,
                va_end=leaf.va_end,
                pa_start=prev.pa_start,
                pa_end=leaf.pa_end,
                flags_key=prev.flags_key,
                flags_str=prev.flags_str,
            )
        else:
            merged.append(leaf)

    out: List[VmapRegion] = []
    cur = region_start & U64_MASK
    report_end = merged[-1].va_end

    for m in merged:
        if cur < m.va_start:
            out.append(VmapRegion(va_start=cur, va_end=m.va_start, kind="unmapped"))
        is_identity = (m.va_start == m.pa_start) and (m.va_end == m.pa_end)
        out.append(
            VmapRegion(
                va_start=m.va_start,
                va_end=m.va_end,
                kind="identity" if is_identity else "mapped",
                pa_start=m.pa_start,
                pa_end=m.pa_end,
                flags_str=m.flags_str,
            )
        )
        cur = m.va_end

    if cur < report_end:
        out.append(VmapRegion(va_start=cur, va_end=report_end, kind="unmapped"))
    return out

def print_vmap(title: str, regions: List[VmapRegion]) -> None:
    print(title + "\n")
    for r in regions:
        if r.kind == "unmapped":
            print(f"0x{r.va_start:x}-0x{r.va_end:x} unmapped")
        elif r.kind == "identity":
            print(f"0x{r.va_start:x}-0x{r.va_end:x} mapped identity, flags {r.flags_str}")
        else:
            print(f"0x{r.va_start:x}-0x{r.va_end:x} mapped to 0x{r.pa_start:x}-0x{r.pa_end:x} flags {r.flags_str}")
    print("")

# ========= main =========

def main() -> int:
    ap = argparse.ArgumentParser(description="AArch64 stage-1 page-table walker (4KB) over QEMU dump.bin")
    ap.add_argument("--dump", default="dump.bin", help="Path to dump (default: dump.bin)")
    ap.add_argument("--mem-base", required=True, type=parse_int, help="Physical address corresponding to file offset 0")
    ap.add_argument("--tcr", type=parse_int, help="TCR_EL1 value (for TTBR selection/start level)")
    ap.add_argument("--ttbr0", type=parse_int, help="TTBR0_EL1 base PA")
    ap.add_argument("--ttbr1", type=parse_int, help="TTBR1_EL1 base PA")
    ap.add_argument("--va", type=parse_int, help="Test virtual address")
    ap.add_argument("--print", dest="print_mode", choices=["vmap"], help="Print virtual memory map")
    ap.add_argument("--trace", action="store_true", help="Interactive mode: ask TCR/TTBR0/TTBR1/VA")
    args = ap.parse_args()

    r = DumpReader(args.dump, args.mem_base)

    if args.print_mode == "vmap":
        if args.tcr is None or args.ttbr0 is None or args.ttbr1 is None:
            ap.error("Provide --tcr, --ttbr0, --ttbr1 for --print vmap")

        tcr = TcrCfg.from_tcr(args.tcr)

        # TTBR0 region (lower range)
        if args.ttbr0 != 0:
            tsz = tcr.t0sz
            va_bits = va_bits_from_tsz(tsz)
            start_lvl = start_level_for_4k(va_bits)
            region_start = 0
            root_pa = args.ttbr0 & TABLE_NEXT_MASK
            leaves, errors = scan_page_tables(r, root_pa, start_lvl, region_start)
            regions = group_regions(leaves, region_start)
            print_vmap("Mapped memory regions (TTBR0):", regions)
            if errors:
                print(f"Warnings (TTBR0): {len(errors)} read/scan issues. First: {errors[0]}\n")

        # TTBR1 region (upper range)
        if args.ttbr1 != 0:
            tsz = tcr.t1sz
            va_bits = va_bits_from_tsz(tsz)
            start_lvl = start_level_for_4k(va_bits)
            region_start = (1 << 64) - (1 << (64 - tsz))
            root_pa = args.ttbr1 & TABLE_NEXT_MASK
            leaves, errors = scan_page_tables(r, root_pa, start_lvl, region_start)
            regions = group_regions(leaves, region_start)
            print_vmap("Mapped memory regions (TTBR1):", regions)
            if errors:
                print(f"Warnings (TTBR1): {len(errors)} read/scan issues. First: {errors[0]}\n")

        return 0

    if args.trace:
        tcr = parse_int(input("TCR_EL1: "))
        ttbr0 = parse_int(input("TTBR0_EL1 (PA): "))
        ttbr1 = parse_int(input("TTBR1_EL1 (PA): "))
        va = parse_int(input("Test VA: "))
    else:
        if args.tcr is None or args.ttbr0 is None or args.ttbr1 is None or args.va is None:
            ap.error("Provide --tcr, --ttbr0, --ttbr1, --va (or use --trace)")
        tcr, ttbr0, ttbr1, va = args.tcr, args.ttbr0, args.ttbr1, args.va

    steps, out_pa, perms, status = walk(r, tcr, ttbr0, ttbr1, va)
    print_log(steps, status, out_pa, perms)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())