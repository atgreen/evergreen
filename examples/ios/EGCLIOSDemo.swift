// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

import Combine
import Foundation
import SwiftUI

@MainActor
final class LispRuntime: ObservableObject {
    @Published var message = "Lisp has not started"
    @Published var accepted = false
    @Published var active = false

    func start() {
        guard !accepted else { return }
        do {
            guard let resources = Bundle.main.resourceURL else {
                message = "Application resources are unavailable"
                return
            }
            let source = resources.appendingPathComponent("Lisp", isDirectory: true).path
            let data = try FileManager.default.url(for: .applicationSupportDirectory,
                in: .userDomainMask, appropriateFor: nil, create: true)
                .appendingPathComponent("EGCLIOSDemo", isDirectory: true).path
            let result = source.withCString { sourcePointer in
                data.withCString { dataPointer in
                    "app.lisp".withCString { entryPointer in
                        var config = egcl_ios_config(
                            abi_version: UInt32(EGCL_IOS_ABI_VERSION),
                            struct_size: UInt32(MemoryLayout<egcl_ios_config>.size),
                            source_directory: sourcePointer,
                            writable_directory: dataPointer,
                            entry_file: entryPointer)
                        return egcl_ios_start(&config)
                    }
                }
            }
            if result != 0 { message = diagnostic() }
            refresh()
        } catch { message = error.localizedDescription }
    }

    func stop() {
        _ = egcl_ios_request_stop()
        refresh()
    }

    func diagnostic() -> String {
        let required = egcl_ios_copy_error(nil, 0)
        guard let needed = Int(exactly: required), needed > 0 else { return "Runtime diagnostic unavailable" }
        var bytes = [CChar](repeating: 0, count: needed)
        _ = bytes.withUnsafeMutableBufferPointer { buffer in
            egcl_ios_copy_error(buffer.baseAddress, numericCast(buffer.count))
        }
        return bytes.withUnsafeBufferPointer { buffer in
            String(cString: buffer.baseAddress!)
        }
    }

    func refresh() {
        var status = egcl_ios_status()
        guard egcl_ios_status(&status) == 0 else { return }
        accepted = status.state != UInt32(EGCL_IOS_IDLE)
        active = accepted && status.state != UInt32(EGCL_IOS_FINISHED)
            && status.state != UInt32(EGCL_IOS_FAILED)
        switch status.state {
        case UInt32(EGCL_IOS_IDLE): break
        case UInt32(EGCL_IOS_STARTING): message = "Starting Lisp worker"
        case UInt32(EGCL_IOS_RUNNING): message = "Lisp worker active"
        case UInt32(EGCL_IOS_ROOT_COMPLETED): message = "Root returned; Lisp attachments remain active"
        case UInt32(EGCL_IOS_STOPPING):
            let detail = diagnostic()
            message = detail.isEmpty ? "Cooperative stop requested; waiting for Lisp" : detail
        case UInt32(EGCL_IOS_FINISHED): message = "Lisp finished with code \(status.exit_code)"
        default: message = diagnostic()
        }
    }
}

@main
@MainActor
struct EGCLIOSDemoApp: App {
    @StateObject private var runtime = LispRuntime()
    private let updates = Timer.publish(every: 0.2, on: .main, in: .common).autoconnect()

    var body: some Scene {
        WindowGroup {
            VStack(spacing: 20) {
                Text(runtime.message)
                Button("Start Lisp") { runtime.start() }.disabled(runtime.accepted)
                Button("Request stop") { runtime.stop() }.disabled(!runtime.active)
            }
            .padding()
            .onReceive(updates) { _ in runtime.refresh() }
        }
    }
}
