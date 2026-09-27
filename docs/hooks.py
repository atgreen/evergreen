"""Generate CLI help at build time from the CLI's own source of truth."""
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
MARKER = '<!-- generated-cli-help -->'


def cli_help(source):
    match = re.search(r"pub fn help_text\(\) -> &'static str\s*\{\s*concat!\((.*?)\)\s*\}", source, re.S)
    if not match:
        raise ValueError('CLI help source changed: update docs/hooks.py before publishing')
    literals = re.findall(r'"(?:[^"\\]|\\.)*"', match[1])
    remainder = re.sub(r'"(?:[^"\\]|\\.)*"', '', match[1])
    if not literals or remainder.strip(' \t\n\r,'):
        raise ValueError('CLI help must contain only string literals')
    return ''.join(json.loads(literal) for literal in literals)


def on_page_markdown(markdown, page, config, files):
    if MARKER not in markdown:
        return markdown
    help_text = cli_help((ROOT / 'crates/torcl/src/cli.rs').read_text())
    return markdown.replace(MARKER, '```text\n' + help_text + '```')
