"""Matrix regression tests require no gateway or CLI compilation."""
import unittest

from check_isolation_tool_matrix import check, check_repository


class IsolationToolMatrixTests(unittest.TestCase):
    policy = 'ToolPolicy { name: "run" }, ToolPolicy { name: "desktop" }, ToolPolicy { name: "fork" }'
    disposition = '"desktop" => HostAuthority,'
    fork = 'pub const DEFERRED_TOOLS: [&str; 1] = ["fork"];'
    matrix = "\n".join([
        "| `run` | A ExecutionBackend |",
        "| `desktop` | Refused: HostAuthority |",
        "| `fork` | Refused: ForkNextRelease |",
    ])

    def validate(self, matrix):
        return check(matrix, self.policy, self.disposition, self.fork)

    def test_repository_matrix(self):
        self.assertEqual(check_repository(), [])

    def test_unique_matrix(self):
        self.assertEqual(self.validate(self.matrix), [])

    def test_duplicate_identical_row(self):
        errors = self.validate(self.matrix + "\n| `run` | A ExecutionBackend |")
        self.assertTrue(any("duplicate tool run" in error for error in errors))

    def test_duplicate_conflicting_row(self):
        errors = self.validate(self.matrix + "\n| `fork` | A ExecutionBackend |")
        self.assertTrue(any("duplicate tool fork" in error for error in errors))
        self.assertTrue(any("refusal contradicts code" in error for error in errors))

    def test_missing_row_and_wrong_refusal_category(self):
        self.assertTrue(self.validate(self.matrix.replace("| `run` | A ExecutionBackend |", "")))
        self.assertTrue(self.validate(self.matrix.replace("ForkNextRelease", "HostAuthority")))

    def test_single_row_cannot_claim_execution_and_refusal(self):
        self.assertTrue(self.validate(self.matrix.replace("Refused: ForkNextRelease", "A ExecutionBackend; Refused: ForkNextRelease")))

    def test_refusal_first_cannot_hide_executable_disposition(self):
        for route in ["A ExecutionBackend", "E WorkerPlanFactory + WorkerLauncher/WorkerSession",
                      "Resource → original handler", "AuthorizedFile", "Host owner-checked query",
                      "Original gateway handler"]:
            with self.subTest(route=route):
                errors = self.validate(self.matrix.replace("Refused: ForkNextRelease", f"Refused: ForkNextRelease; {route}"))
                self.assertTrue(any("mixed with executable" in error for error in errors))

    def test_unknown_refusal_and_unparseable_inventory_fail_closed(self):
        self.assertTrue(self.validate(self.matrix.replace("A ExecutionBackend", "Refused: NewRestriction")))
        self.assertTrue(check(self.matrix, "", self.disposition, self.fork))


if __name__ == "__main__":
    unittest.main()
