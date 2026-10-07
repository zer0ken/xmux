import unittest
from unittest.mock import patch

import record


class ScreenTests(unittest.TestCase):
    def test_private_status_query_preserves_the_recorded_screen(self):
        with patch.object(record.pty, "fork", return_value=(1, 0)), \
                patch.object(record.fcntl, "ioctl"), \
                patch.object(record.threading.Thread, "start"):
            term = record.Term(20, 4)
        reports = []
        term.screen.write_process_input = reports.append
        term.stream.feed(b"before\x1b[?996nafter\x1b[6n")
        screen = term.screen
        self.assertEqual(screen.display[0].rstrip(), "beforeafter")
        self.assertEqual((screen.cursor.x, screen.cursor.y), (11, 0))
        self.assertEqual(reports, ["\x1b[1;12R"])


if __name__ == "__main__":
    unittest.main()
