# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Guard the source-derived CLI reference against silent parser drift."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('docs_hook', ROOT / 'docs/hooks.py')
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

class CliReferenceTests(unittest.TestCase):
    def test_extracts_help_without_other_source_strings(self):
        source = '''pub fn help_text() -> &'static str {
    concat!("Usage: egcl\\n", "  --eval EXPR\\n",)
}
pub fn unrelated() { println!("not documentation"); }
'''
        self.assertEqual(hook.cli_help(source), 'Usage: egcl\n  --eval EXPR\n')

    def test_changed_source_shape_fails_instead_of_publishing_empty_help(self):
        with self.assertRaises(ValueError):
            hook.cli_help('pub fn help_text() -> String { build_help() }')

    def test_current_source_has_expected_public_interface(self):
        text = hook.cli_help((ROOT / 'crates/egcl/src/cli.rs').read_text())
        self.assertTrue(text.startswith('Usage: egcl'))
        self.assertIn('--no-init', text)
        self.assertIn('*command-line-args*', text)

if __name__ == '__main__': unittest.main()
