"""Source regression checks; no native Windows/MSI execution.

Run with Python 3; optionally pass another snapshot to check a previous revision.
"""

from pathlib import Path
import sys
import unittest
import xml.etree.ElementTree as ET


SNAPSHOT = Path(sys.argv.pop(1)) if len(sys.argv) > 1 else Path(__file__).with_name("installer-ui.wxs")
NS = {"w": "http://wixtoolset.org/schemas/v4/wxs"}


class InstallerUiTests(unittest.TestCase):
    def setUp(self):
        self.root = ET.parse(SNAPSHOT).getroot()

    def test_files_in_use_ok_requests_restart_manager_and_returns(self):
        # MSIRMSHUTDOWN=2 shuts down nothing if any process is not restart-registered.
        # The documented MsiRMFilesInUse choice uses RMShutdownAndRestart instead.
        dialog = self.root.find('.//w:Dialog[@Id="MsiRMFilesInUse"]', NS)
        self.assertIsNotNone(dialog)
        self.assertEqual(dialog.get("KeepModeless"), "yes")
        button = dialog.find('w:Control[@Id="OK"]', NS)
        self.assertIsNotNone(button)
        publications = button.findall("w:Publish", NS)
        self.assertEqual(
            [(p.get("Event"), p.get("Value"), p.get("Order")) for p in publications],
            [("RMShutdownAndRestart", "0", "1"), ("EndDialog", "Return", "2")],
        )
        self.assertFalse(any(p.get("Property") == "MSIRMSHUTDOWN" for p in publications))

    def test_files_in_use_cancel_exits_without_shutdown(self):
        button = self.root.find('.//w:Dialog[@Id="MsiRMFilesInUse"]/w:Control[@Id="Cancel"]', NS)
        self.assertIsNotNone(button)
        self.assertEqual(button.get("Cancel"), "yes")
        self.assertEqual(
            [p.attrib for p in button.findall("w:Publish", NS)],
            [{"Event": "EndDialog", "Value": "Exit"}],
        )

    def test_authored_dialogs_require_full_ui_and_quiet_removal_stays_quiet(self):
        shows = self.root.findall(".//w:Show", NS)
        self.assertTrue(shows)
        for show in shows:
            self.assertIn("UILevel = 5", show.get("Condition", ""))
        settings = self.root.find(
            './/w:Component[@Id="CraftInstallerSettingsEntry"]/w:RegistryKey[@Root="HKLM"]'
            r'[@Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Craft.filmcraft"]', NS
        )
        self.assertIsNotNone(settings)
        commands = {p.get("Name"): p.get("Value") for p in settings.findall("w:RegistryValue", NS)}
        for name, ui in [("UninstallString", "/qf"), ("QuietUninstallString", "/qn")]:
            self.assertEqual(commands[name], f'"[$(var.CraftMsiSystemFolder)]msiexec.exe" /x [ProductCode] {ui} /norestart')


if __name__ == "__main__":
    unittest.main()
