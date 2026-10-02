import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let l = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as! [[String: Any]]
for w in l where (w["kCGWindowOwnerPID"] as? Int) == pid { print(w["kCGWindowNumber"]!); break }
