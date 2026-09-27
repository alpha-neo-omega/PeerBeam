import Cocoa
import FlutterMacOS

@main
class AppDelegate: FlutterAppDelegate {
  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return true
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }

  /// Bring the window back when the Dock icon is clicked with none on screen.
  ///
  /// Close-to-tray hides the window with `orderOut:` rather than closing it.
  /// AppKit's default reopen handling deminiaturises windows but will not
  /// order a hidden one back, so without this the Dock icon is inert. Paired
  /// with a menu-bar icon the user cannot see, that left the app running with
  /// no window and no way back into it short of killing the process.
  override func applicationShouldHandleReopen(
    _ sender: NSApplication,
    hasVisibleWindows flag: Bool
  ) -> Bool {
    if !flag {
      for window in sender.windows {
        window.makeKeyAndOrderFront(self)
      }
      sender.activate(ignoringOtherApps: true)
    }
    return true
  }
}
