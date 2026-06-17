#!/usr/bin/env python3
"""
Merge a skill folder (SKILL.md + references/*.md) into a single markdown file.

Usage:
    python3 skill_merge.py <skill_folder> [output_file]
    python3 skill_merge.py --test

If output_file is omitted, prints to stdout.
Implements the spec at docs/skill-md-merge-spec.md.
"""

import re
import sys
import tempfile
from pathlib import Path

# ---------------------------------------------------------------------------
# Step 2: frontmatter parsing
# ---------------------------------------------------------------------------

_FM_KEY_RE = re.compile(r'^([a-zA-Z][a-zA-Z0-9_-]*):\s*(.*)$')


def _parse_frontmatter(text: str) -> tuple[dict[str, str], str]:
    """Strip BOM, normalize line endings, parse frontmatter. Return (fields, body)."""
    text = text.lstrip('﻿')
    text = text.replace('\r\n', '\n').replace('\r', '\n')

    # Opening delimiter must be exactly '---' on its own line.
    if not text.startswith('---\n'):
        return {}, text

    # Closing delimiter: '\n---' followed by '\n' or end of string.
    search_from = 4
    close = -1
    while True:
        idx = text.find('\n---', search_from)
        if idx == -1:
            break
        after = idx + 4
        if after >= len(text) or text[after] == '\n':
            close = idx
            break
        search_from = idx + 1

    if close == -1:
        return {}, text

    fm_block = text[4:close]
    rest = text[close + 4:]
    body = rest.lstrip('\n')

    fields: dict[str, str] = {}
    for line in fm_block.splitlines():
        m = _FM_KEY_RE.match(line)
        if not m:
            continue
        key, value = m.group(1), m.group(2).strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
            value = value[1:-1]
        fields[key] = value  # last occurrence wins

    return fields, body


# ---------------------------------------------------------------------------
# Step 3: local markdown link normalization
# ---------------------------------------------------------------------------

_LINK_RE = re.compile(r'\[([^\]]*)\]\(([^)]*)\)')
_LOCAL_TARGET_RE = re.compile(r'^(?!https?://).*\.md(#[^)]*)?$')

# Strips allowed leading wrappers (blockquote, list marker, checkbox).
_LEADING_WRAPPER_RE = re.compile(
    r'^(\s*'
    r'(>\s+)*'                   # blockquote markers
    r'(\d+\.\s+|-\s+|\*\s+)?'   # ordered / unordered list marker
    r'(\[[ xX]\]\s+)?'          # checkbox
    r')'
)
_TRAILING_PUNCT_RE = re.compile(r'[.,;:!?]$')


def _label_from_target(target: str) -> str:
    path = target.split('#')[0]
    stem = Path(path).stem
    return stem.replace('-', ' ').replace('_', ' ')


def _substitute_link(display: str, target: str) -> str:
    if '/' in display or display.endswith('.md'):
        return f'the "{_label_from_target(target)}" reference below'
    return display


def _normalize_links_in_line(line: str) -> str | None:
    """Return normalized line, or None if it should be deleted."""
    local_spans = [m for m in _LINK_RE.finditer(line) if _LOCAL_TARGET_RE.match(m.group(2))]
    if not local_spans:
        return line

    # Bare-link check: strip allowed wrappers, then compare to the single link token.
    stripped = _LEADING_WRAPPER_RE.sub('', line)
    stripped = _TRAILING_PUNCT_RE.sub('', stripped).strip()
    if len(local_spans) == 1 and stripped == local_spans[0].group(0):
        return None

    result = line
    for m in reversed(local_spans):
        result = result[:m.start()] + _substitute_link(m.group(1), m.group(2)) + result[m.end():]
    return result


def _normalize_links(body: str) -> str:
    lines = []
    for line in body.splitlines():
        out = _normalize_links_in_line(line)
        if out is not None:
            lines.append(out)
    return '\n'.join(lines)


# ---------------------------------------------------------------------------
# Step 1: collect files
# ---------------------------------------------------------------------------

def _collect_files(folder: Path) -> tuple[Path, list[Path]]:
    """Return (main_file, sorted reference files). Raises ValueError if no main file."""
    main_file = None
    for name in ('SKILL.md', 'skill.md', 'Skill.md'):
        candidate = folder / name
        if candidate.is_file():
            main_file = candidate
            break

    if main_file is None:
        raise ValueError(
            f"No SKILL.md found in {folder} (looked for SKILL.md, skill.md, Skill.md)"
        )

    refs_dir = folder / 'references'
    ref_files: list[Path] = []
    if refs_dir.is_dir():
        ref_files = sorted(p for p in refs_dir.iterdir() if p.is_file() and p.suffix == '.md')

    return main_file, ref_files


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def merge_skill_folder(folder: Path) -> dict[str, str]:
    """
    Merge a skill folder into a single prompt.

    Returns {'name': str, 'description': str, 'prefix': str}.
    """
    main_file, ref_files = _collect_files(folder)

    fields, body = _parse_frontmatter(main_file.read_text(encoding='utf-8'))
    parts = [_normalize_links(body).rstrip()]

    for ref_path in ref_files:
        _, ref_body = _parse_frontmatter(ref_path.read_text(encoding='utf-8'))
        ref_body = _normalize_links(ref_body).rstrip()
        parts.append(f"\n\n---\n\n## Reference: references/{ref_path.name}\n\n{ref_body}")

    return {
        'name': fields.get('name', ''),
        'description': fields.get('description', ''),
        'prefix': ''.join(parts) + '\n',
    }


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def _check(label: str, got, expected) -> bool:
    if got == expected:
        return True
    print(f'  FAIL  {label}')
    print(f'        expected: {expected!r}')
    print(f'        got:      {got!r}')
    return False


def _test_link_normalization() -> int:
    """Golden cases from spec §Step 3. Returns failure count."""
    cases = [
        ('case 1',
         '**For detailed templates**: See [references/ad-copy-templates.md](references/ad-copy-templates.md)',
         '**For detailed templates**: See the "ad copy templates" reference below'),
        ('case 2',
         'For tracking setup, see [references/conversion-tracking.md](references/conversion-tracking.md)',
         'For tracking setup, see the "conversion tracking" reference below'),
        ('case 3',
         'See the [ad copy reference](references/ad-copy-templates.md) and the [audience guide](references/audience-targeting.md)',
         'See the ad copy reference and the audience guide'),
        ('case 4',
         '| Google Ads | [setup guide](references/platform-setup-checklists.md) | High-intent |',
         '| Google Ads | setup guide | High-intent |'),
        ('case 5',
         'Read the [overview](references/overview.md) before running ads on [Meta](https://facebook.com).',
         'Read the overview before running ads on [Meta](https://facebook.com).'),
        ('case 6 — bare, no wrapper',
         '[references/ad-copy-templates.md](references/ad-copy-templates.md)',
         None),
        ('case 7 — bare, unordered list',
         '- [audience-targeting.md](references/audience-targeting.md)',
         None),
        ('case 8 — bare, ordered list',
         '1. [conversion-tracking.md](references/conversion-tracking.md)',
         None),
        ('case 9 — bare, blockquote',
         '> [platform-setup-checklists.md](references/platform-setup-checklists.md)',
         None),
        ('case 10 — bare, checkbox',
         '- [ ] [audience-targeting.md](references/audience-targeting.md)',
         None),
        ('case 11 — prose + list marker',
         '- Audience targeting strategies — see [audience-targeting.md](references/audience-targeting.md)',
         '- Audience targeting strategies — see the "audience targeting" reference below'),
        ('case 12 — prose before link',
         'For conversion pixel installation and event setup: See [references/conversion-tracking.md](references/conversion-tracking.md)',
         'For conversion pixel installation and event setup: See the "conversion tracking" reference below'),
        ('case 13 — prose after link',
         'The [platform guide](references/platform-setup-checklists.md) covers Google, Meta, LinkedIn, and TikTok.',
         'The platform guide covers Google, Meta, LinkedIn, and TikTok.'),
        ('case 14 — human-readable display, prose after',
         '- [Conversion Tracking](references/conversion-tracking.md): pixel setup and event configuration',
         '- Conversion Tracking: pixel setup and event configuration'),
        ('case 15 — checkbox + prose, keep',
         '- [ ] [Conversion Tracking](references/conversion-tracking.md): verify pixel fires',
         '- [ ] Conversion Tracking: verify pixel fires'),
        ('case 16 — non-.md, untouched',
         '[image.png](./image.png)',
         '[image.png](./image.png)'),
        ('case 17 — anchor link, prose',
         'See [tracking setup](references/conversion-tracking.md#pixel-setup) for details.',
         'See tracking setup for details.'),
        ('case 18 — bare anchor link, deleted',
         '- [conversion-tracking.md#pixel-setup](references/conversion-tracking.md#pixel-setup)',
         None),
    ]
    failures = 0
    for label, inp, expected in cases:
        if not _check(f'link/{label}', _normalize_links_in_line(inp), expected):
            failures += 1
    return failures


def _test_frontmatter() -> int:
    """Tests for _parse_frontmatter. Returns failure count."""
    failures = 0

    # No frontmatter
    fields, body = _parse_frontmatter('# Hello\nworld\n')
    failures += 0 if _check('fm/no-frontmatter fields', fields, {}) else 1
    failures += 0 if _check('fm/no-frontmatter body', body, '# Hello\nworld\n') else 1

    # Normal case
    fields, body = _parse_frontmatter('---\nname: ads\ndescription: "Buy stuff"\n---\n\n# Body\n')
    failures += 0 if _check('fm/normal name', fields.get('name'), 'ads') else 1
    failures += 0 if _check('fm/normal description', fields.get('description'), 'Buy stuff') else 1
    failures += 0 if _check('fm/normal body', body, '# Body\n') else 1

    # BOM stripped
    fields, body = _parse_frontmatter('﻿---\nname: bom\n---\nbody\n')
    failures += 0 if _check('fm/bom', fields.get('name'), 'bom') else 1

    # CRLF normalized
    fields, body = _parse_frontmatter('---\r\nname: crlf\r\n---\r\nbody\r\n')
    failures += 0 if _check('fm/crlf name', fields.get('name'), 'crlf') else 1
    failures += 0 if _check('fm/crlf body', body, 'body\n') else 1

    # Nested/unsupported YAML lines silently ignored
    fields, body = _parse_frontmatter('---\nname: x\nmetadata:\n  version: 1.0\n---\nbody\n')
    failures += 0 if _check('fm/nested name', fields.get('name'), 'x') else 1
    failures += 0 if _check('fm/nested metadata empty', fields.get('metadata'), '') else 1

    # Single-quoted value
    fields, _ = _parse_frontmatter("---\nname: 'quoted'\n---\n")
    failures += 0 if _check('fm/single-quote', fields.get('name'), 'quoted') else 1

    # Duplicate keys: last wins
    fields, _ = _parse_frontmatter('---\nname: first\nname: second\n---\n')
    failures += 0 if _check('fm/duplicate-key', fields.get('name'), 'second') else 1

    # Missing closing delimiter — treated as no frontmatter
    fields, body = _parse_frontmatter('---\nname: x\n')
    failures += 0 if _check('fm/no-closing-delimiter fields', fields, {}) else 1

    # Opening '----' not treated as frontmatter
    fields, body = _parse_frontmatter('----\nname: x\n----\nbody\n')
    failures += 0 if _check('fm/four-dashes fields', fields, {}) else 1
    failures += 0 if _check('fm/four-dashes body start', body[:4], '----') else 1

    # Closing '---extra' not treated as closing delimiter
    text = '---\nname: x\n---extra\n---\nbody\n'
    fields, body = _parse_frontmatter(text)
    failures += 0 if _check('fm/closing-extra name', fields.get('name'), 'x') else 1
    failures += 0 if _check('fm/closing-extra body', body, 'body\n') else 1

    return failures


def _test_collection_and_assembly() -> int:
    """End-to-end tests for Steps 1, 2, and 5. Returns failure count."""
    failures = 0

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)

        # No SKILL.md → error
        try:
            _collect_files(root)
            failures += 0 if _check('collect/no-skill-md', False, True) else 1
        except ValueError:
            pass  # expected

        # SKILL.md found, no references/
        (root / 'SKILL.md').write_text('---\nname: test\ndescription: "desc"\n---\n\nBody text.\n')
        result = merge_skill_folder(root)
        failures += 0 if _check('assemble/name', result['name'], 'test') else 1
        failures += 0 if _check('assemble/description', result['description'], 'desc') else 1
        failures += 0 if _check('assemble/no-refs prefix', result['prefix'], 'Body text.\n') else 1

        # skill.md fallback
        (root / 'SKILL.md').unlink()
        (root / 'skill.md').write_text('---\nname: lower\n---\n\nLower body.\n')
        result = merge_skill_folder(root)
        failures += 0 if _check('collect/skill-md-lower', result['name'], 'lower') else 1
        (root / 'skill.md').unlink()

        # References sorted alphabetically, headers correct, separator format
        (root / 'SKILL.md').write_text('---\nname: x\n---\n\nMain body.\n')
        refs = root / 'references'
        refs.mkdir()
        (refs / 'b-ref.md').write_text('B content.\n')
        (refs / 'a-ref.md').write_text('A content.\n')
        (refs / 'notes.txt').write_text('ignored\n')  # non-.md ignored

        result = merge_skill_folder(root)
        expected_prefix = (
            'Main body.\n\n'
            '---\n\n'
            '## Reference: references/a-ref.md\n\n'
            'A content.\n\n'
            '---\n\n'
            '## Reference: references/b-ref.md\n\n'
            'B content.\n'
        )
        failures += 0 if _check('assemble/with-refs', result['prefix'], expected_prefix) else 1

        # Reference file with frontmatter stripped
        (refs / 'a-ref.md').write_text('---\ntitle: A\n---\n\nA content.\n')
        result = merge_skill_folder(root)
        failures += 0 if _check('assemble/ref-fm-stripped', 'A content.' in result['prefix'], True) else 1
        failures += 0 if _check('assemble/ref-fm-title-absent', 'title: A' not in result['prefix'], True) else 1

        # Empty reference file — header present, empty body
        (refs / 'a-ref.md').write_text('')
        result = merge_skill_folder(root)
        failures += 0 if _check('assemble/empty-ref-header', '## Reference: references/a-ref.md' in result['prefix'], True) else 1

        # Trailing newline always present
        failures += 0 if _check('assemble/trailing-newline', result['prefix'].endswith('\n'), True) else 1

    return failures


def _run_tests() -> None:
    f1 = _test_link_normalization()
    f2 = _test_frontmatter()
    f3 = _test_collection_and_assembly()
    total = f1 + f2 + f3
    passed = (18 + 16 + 10) - total  # approximate; actual count from test counts
    print(f'\nlink: {18 - f1}/18  frontmatter: {16 - f2}/16  assembly: {10 - f3}/10')
    if total:
        sys.exit(1)
    else:
        print('All tests passed.')


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    if len(sys.argv) == 2 and sys.argv[1] == '--test':
        _run_tests()
        return

    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    folder = Path(sys.argv[1]).expanduser().resolve()
    if not folder.is_dir():
        print(f'Error: {folder} is not a directory', file=sys.stderr)
        sys.exit(1)

    result = merge_skill_folder(folder)

    if len(sys.argv) >= 3:
        out = Path(sys.argv[2])
        out.write_text(result['prefix'], encoding='utf-8')
        print(f"Written to {out}  (name={result['name']!r})")
    else:
        print(result['prefix'], end='')


if __name__ == '__main__':
    main()
