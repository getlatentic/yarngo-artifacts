// YarngoTokens.swift — Yarngo Design Tokens v1.0
import SwiftUI

public enum Yarngo {
    public enum Color {
        // Brand
        public static let orange     = SwiftUI.Color(hex: 0xFF8A1F)
        public static let orange600  = SwiftUI.Color(hex: 0xE6740F)
        public static let orange700  = SwiftUI.Color(hex: 0xBF5C08)
        public static let ink        = SwiftUI.Color(hex: 0x171717)
        public static let warmWhite  = SwiftUI.Color(hex: 0xFFF9F2)
        public static let leaf       = SwiftUI.Color(hex: 0x287A57)
        public static let signal     = SwiftUI.Color(hex: 0x596DF8)
        // Semantic — light
        public static let bg         = SwiftUI.Color(hex: 0xFFF9F2)
        public static let surface    = SwiftUI.Color(hex: 0xFFFFFF)
        public static let border     = SwiftUI.Color(hex: 0xEBE4D9)
        public static let text       = SwiftUI.Color(hex: 0x171717)
        public static let textMuted  = SwiftUI.Color(hex: 0x5F594F)
        public static let onAccent   = SwiftUI.Color(hex: 0x171717) // Ink on orange, never white
        public static let success    = SwiftUI.Color(hex: 0x287A57)
        public static let warning    = SwiftUI.Color(hex: 0xC97A00)
        public static let danger     = SwiftUI.Color(hex: 0xC7362B)
        // Semantic — dark
        public enum Dark {
            public static let bg        = SwiftUI.Color(hex: 0x121110)
            public static let surface   = SwiftUI.Color(hex: 0x1F1C19)
            public static let border    = SwiftUI.Color(hex: 0x35302A)
            public static let text      = SwiftUI.Color(hex: 0xFFF9F2)
            public static let accent    = SwiftUI.Color(hex: 0xFF9C3D)
        }
    }
    public enum Space {
        public static let x1: CGFloat = 4,  x2: CGFloat = 8,  x3: CGFloat = 12
        public static let x4: CGFloat = 16, x5: CGFloat = 20, x6: CGFloat = 24
        public static let x8: CGFloat = 32, x10: CGFloat = 40, x12: CGFloat = 48
    }
    public enum Radius {
        public static let xs: CGFloat = 4,  sm: CGFloat = 8,  md: CGFloat = 12
        public static let lg: CGFloat = 16, xl: CGFloat = 24, xxl: CGFloat = 32
        public static let full: CGFloat = 999
    }
    public enum Duration {
        public static let instant = 0.12, fast = 0.18, base = 0.24, slow = 0.32, breathe = 1.6
    }
    public enum Font {
        public static func display(_ size: CGFloat, _ weight: SwiftUI.Font.Weight = .semibold) -> SwiftUI.Font {
            .custom("Sora", size: size).weight(weight)
        }
        public static func text(_ size: CGFloat, _ weight: SwiftUI.Font.Weight = .regular) -> SwiftUI.Font {
            .custom("NotoSans", size: size).weight(weight)
        }
    }
    public static let minTouchTarget: CGFloat = 44
}

extension SwiftUI.Color {
    init(hex: UInt32) {
        self.init(.sRGB,
                  red:   Double((hex >> 16) & 0xFF) / 255,
                  green: Double((hex >> 8)  & 0xFF) / 255,
                  blue:  Double( hex        & 0xFF) / 255,
                  opacity: 1)
    }
}
