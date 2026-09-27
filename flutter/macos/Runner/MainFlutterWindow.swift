import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)

    // Display title. PRODUCT_NAME stays lowercase for the bundle/binary name;
    // the names macOS shows the user come from CFBundleName/CFBundleDisplayName
    // in Info.plist, and this is the window's own.
    self.title = "PeerBeam"

    // The nib opens at 1280x720 to match Windows and Linux, which is wider
    // than the old 800x600 and so no longer certain to sit on screen at the
    // nib's hard-coded origin. Centring costs nothing and is correct on every
    // display size, including the 1280x800 panels where the window is nearly
    // full width.
    self.center()

    super.awakeFromNib()
  }
}
