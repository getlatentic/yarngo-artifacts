// YarngoTokens.kt — Yarngo Design Tokens v1.0
package com.yarngo.design

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

object YarngoColor {
    // Brand
    val Orange    = Color(0xFFFF8A1F)
    val Orange600 = Color(0xFFE6740F)
    val Orange700 = Color(0xFFBF5C08)
    val Ink       = Color(0xFF171717)
    val WarmWhite = Color(0xFFFFF9F2)
    val Leaf      = Color(0xFF287A57)
    val Signal    = Color(0xFF596DF8)
    // Semantic — light
    val Bg        = Color(0xFFFFF9F2)
    val Surface   = Color(0xFFFFFFFF)
    val Border    = Color(0xFFEBE4D9)
    val Text      = Color(0xFF171717)
    val TextMuted = Color(0xFF5F594F)
    val OnAccent  = Color(0xFF171717) // Ink on orange, never white
    val Success   = Color(0xFF287A57)
    val Warning   = Color(0xFFC97A00)
    val Danger    = Color(0xFFC7362B)
    // Semantic — dark
    object Dark {
        val Bg      = Color(0xFF121110)
        val Surface = Color(0xFF1F1C19)
        val Border  = Color(0xFF35302A)
        val Text    = Color(0xFFFFF9F2)
        val Accent  = Color(0xFFFF9C3D)
    }
}

object YarngoSpace {
    val x1 = 4.dp; val x2 = 8.dp;  val x3 = 12.dp; val x4 = 16.dp
    val x5 = 20.dp; val x6 = 24.dp; val x8 = 32.dp; val x10 = 40.dp; val x12 = 48.dp
}

object YarngoRadius {
    val xs = 4.dp; val sm = 8.dp; val md = 12.dp
    val lg = 16.dp; val xl = 24.dp; val xxl = 32.dp; val full = 999.dp
}

object YarngoDuration {
    const val Instant = 120; const val Fast = 180; const val Base = 240
    const val Slow = 320; const val Breathe = 1600
}

object YarngoType {
    val Display1 = 64.sp; val Display2 = 48.sp; val H1 = 36.sp; val H2 = 28.sp
    val H3 = 22.sp; val Body = 16.sp; val BodySm = 14.sp; val Label = 12.sp
}

val YARNGO_MIN_TOUCH_TARGET = 44.dp
