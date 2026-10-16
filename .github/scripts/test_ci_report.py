import unittest
from ci_report import report

POSITIVE = 'GPU vs CPU p99 0, mean 0.0' + "\ntest result: ok. 12 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n"


class CoverageReportTests(unittest.TestCase):
    def test_executed_gpu_tests_and_quality_gate_pass(self):
        code, text = report(POSITIVE, "success", "success")
        self.assertEqual(code, 0)
        self.assertIn("12 passed, 0 failed, 1 ignored", text)

    def test_adapter_probe_failure_is_not_parity(self):
        code, text = report(POSITIVE, "failure", "success")
        self.assertEqual(code, 1)
        self.assertIn("probe did not succeed", text)

    def test_runtime_gpu_skip_cannot_pass_with_other_gpu_evidence(self):
        code, text = report(POSITIVE + "\nno GPU adapter; skipping\n", "success", "success")
        self.assertEqual(code, 1)
        self.assertIn("GPU tests skipped at runtime", text)

    def test_quality_gate_failure_is_reported(self):
        code, text = report(POSITIVE, "success", "failure")
        self.assertEqual(code, 1)
        self.assertIn("Full cargo xtask ci did not pass", text)

    def test_no_selected_tests_or_missing_logs_fail(self):
        self.assertEqual(report("", "success", "success")[0], 1)
        self.assertEqual(report('GPU vs CPU p99 0, mean 0.0', "success", "success")[0], 1)

    def test_ignored_platform_case_is_reported_not_runtime_skip(self):
        code, text = report(POSITIVE + "test windows_case ... ignored, no adapter skips\n", "success", "success")
        self.assertEqual(code, 0)
        self.assertIn("GPU runtime-skip messages: 0", text)

    def test_expected_no_adapter_error_without_skip_is_not_runtime_skip(self):
        self.assertEqual(report(POSITIVE + "expected constructor error: no adapter\n", "success", "success")[0], 0)

    def test_no_gpu_execution_evidence_fails(self):
        code, text = report("test result: ok. 12 passed; 0 failed; 0 ignored\n", "success", "success")
        self.assertEqual(code, 1)
        self.assertIn("No successful GPU execution evidence", text)


if __name__ == "__main__":
    unittest.main()
