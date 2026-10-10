# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Guard the source-derived CLI reference against silent parser drift."""
import importlib.util
from pathlib import Path
import unittest

import yaml

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

class PublicationConcurrencyTests(unittest.TestCase):
    def test_previews_cannot_hold_the_publication_lock(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/docs.yml').read_text())
        concurrency = workflow['concurrency']
        self.assertEqual(concurrency['group'],
                         "docs-preview-${{ github.event.pull_request.number || github.run_id }}")
        self.assertEqual(concurrency['cancel-in-progress'],
                         "${{ github.event_name == 'pull_request' }}")
        self.assertNotIn('queue', concurrency)
        self.assertNotIn('concurrency', workflow['jobs']['build'])
        deploy = workflow['jobs']['deploy']
        self.assertIn("github.ref == 'refs/heads/main'", deploy['if'])
        self.assertIn("github.event_name == 'push' || github.event_name == 'workflow_dispatch'",
                      deploy['if'])
        repository = yaml.safe_load((ROOT / '.github/workflows/repository.yml').read_text())
        self.assertEqual(deploy['concurrency'], repository['concurrency'])
        self.assertEqual(deploy['concurrency'],
                         {'group': 'pages', 'cancel-in-progress': False, 'queue': 'max'})

    def test_every_documentation_job_has_a_bounded_runtime(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/docs.yml').read_text())
        for name, job in workflow['jobs'].items():
            with self.subTest(job=name):
                self.assertGreater(job.get('timeout-minutes', 0), 0)
                self.assertLessEqual(job['timeout-minutes'], 20)


if __name__ == '__main__': unittest.main()
