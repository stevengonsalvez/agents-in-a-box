---
name: liquid-glass
description: |
  iOS 26 Liquid Glass design system for SwiftUI. Implements Apple's
  glassmorphism material effects, depth-based layering, and adaptive
  tinting. Follows Apple Human Interface Guidelines for glass materials.

  Use when: (1) Building iOS 26+ SwiftUI interfaces, (2) Implementing
  glassmorphism effects, (3) Creating translucent/frosted UI elements,
  (4) Designing with Apple's Liquid Glass aesthetic,
  (5) User mentions liquid glass, glassmorphism, or frosted glass UI.
---

# Liquid Glass — iOS 26 SwiftUI Design System

## Overview

Liquid Glass is Apple's design language introduced in iOS 26 (2025). It features
translucent, depth-aware materials that react to content behind them. This skill
provides patterns for implementing Liquid Glass effects in SwiftUI.

## Core Principles

1. **Translucency over opacity** — Elements reveal the content beneath them
2. **Depth through layering** — Multiple glass layers create visual hierarchy
3. **Adaptive tinting** — Glass adapts color to surrounding content
4. **Motion and physics** — Elements respond to scroll, tilt, and interaction
5. **Semantic materials** — Use Apple's material types, not hardcoded colors

## SwiftUI Materials

### Built-in Materials (iOS 15+, enhanced iOS 26)

```swift
// Thin material — barely visible, subtle blur
.background(.thinMaterial)

// Regular material — standard glass effect
.background(.regularMaterial)

// Thick material — more opaque, stronger effect
.background(.thickMaterial)

// Ultra-thin material — maximum transparency
.background(.ultraThinMaterial)

// Ultra-thick material — nearly opaque
.background(.ultraThickMaterial)

// Bar material — for navigation/tab bars
.background(.bar)
```

### Liquid Glass Modifier (iOS 26+)

```swift
// New in iOS 26: native liquid glass effect
.glassEffect(.regular)

// With tinting
.glassEffect(.regular.tint(.blue))

// Interactive glass (responds to hover/press)
.glassEffect(.regular.interactive())
```

## Component Patterns

### Glass Card

```swift
struct GlassCard<Content: View>: View {
    let content: () -> Content

    var body: some View {
        content()
            .padding(20)
            .background(.ultraThinMaterial)
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
            .shadow(color: .black.opacity(0.1), radius: 10, y: 5)
            .overlay(
                RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .stroke(.white.opacity(0.2), lineWidth: 0.5)
            )
    }
}

// Usage
GlassCard {
    VStack(alignment: .leading, spacing: 8) {
        Text("Title").font(.headline)
        Text("Subtitle").font(.subheadline).foregroundStyle(.secondary)
    }
}
```

### Glass Navigation Bar

```swift
struct GlassNavBar: View {
    let title: String

    var body: some View {
        HStack {
            Text(title)
                .font(.largeTitle.bold())
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background(.bar)
        .overlay(alignment: .bottom) {
            Divider().opacity(0.3)
        }
    }
}
```

### Glass Tab Bar

```swift
struct GlassTabBar: View {
    @Binding var selection: Int
    let items: [(icon: String, label: String)]

    var body: some View {
        HStack {
            ForEach(items.indices, id: \.self) { index in
                Button {
                    withAnimation(.spring(response: 0.3)) {
                        selection = index
                    }
                } label: {
                    VStack(spacing: 4) {
                        Image(systemName: items[index].icon)
                            .font(.system(size: 20))
                        Text(items[index].label)
                            .font(.caption2)
                    }
                    .foregroundStyle(selection == index ? .primary : .secondary)
                    .frame(maxWidth: .infinity)
                }
            }
        }
        .padding(.vertical, 8)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .padding(.horizontal, 20)
    }
}
```

### Glass Button

```swift
struct GlassButton: View {
    let title: String
    let icon: String?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if let icon {
                    Image(systemName: icon)
                }
                Text(title).fontWeight(.medium)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
            .background(.ultraThinMaterial)
            .clipShape(Capsule())
            .overlay(Capsule().stroke(.white.opacity(0.2), lineWidth: 0.5))
        }
    }
}
```

### Glass Sheet / Modal

```swift
struct GlassSheet<Content: View>: View {
    let content: () -> Content

    var body: some View {
        content()
            .frame(maxWidth: .infinity)
            .padding(24)
            .background(.regularMaterial)
            .clipShape(RoundedRectangle(cornerRadius: 32, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 32, style: .continuous)
                    .stroke(.white.opacity(0.15), lineWidth: 0.5)
            )
            .shadow(color: .black.opacity(0.2), radius: 20, y: 10)
            .padding(16)
    }
}
```

## Color & Tinting

### Adaptive Tinting

```swift
// Glass that picks up color from content beneath
.background(.ultraThinMaterial)
.environment(\.colorScheme, .dark) // Force dark glass

// Tinted glass
ZStack {
    Color.blue.opacity(0.15)
    content
}
.background(.ultraThinMaterial)
```

### Vibrancy

```swift
// Text that adapts to material behind it
Text("Label")
    .foregroundStyle(.primary) // Adapts to material

Text("Secondary")
    .foregroundStyle(.secondary) // Reduced prominence

// Use semantic colors — they adapt to materials
Text("Vibrant")
    .foregroundStyle(.primary)
    .environment(\.backgroundMaterial, .ultraThinMaterial)
```

## Layout Patterns

### Depth Layering

```
+-----------------------------------------+
| Background (image, gradient, video)     |  Layer 0: Content
+-----------------------------------------+
| Ultra-thin material overlay             |  Layer 1: Ambient glass
+-----------------------------------------+
| Regular material cards                  |  Layer 2: Content glass
+-----------------------------------------+
| Thick material controls                 |  Layer 3: Interactive glass
+-----------------------------------------+
| Bar material navigation                 |  Layer 4: Chrome glass
+-----------------------------------------+
```

**Rule**: Each layer up uses a thicker material. Never put thin material on top of thick.

### Scroll-Aware Glass

```swift
struct ScrollGlassHeader: View {
    @State private var scrollOffset: CGFloat = 0

    var body: some View {
        ZStack(alignment: .top) {
            ScrollView {
                // Content with offset tracking
                GeometryReader { geo in
                    Color.clear.preference(
                        key: ScrollOffsetKey.self,
                        value: geo.frame(in: .named("scroll")).minY
                    )
                }
                .frame(height: 0)

                // Actual content
                LazyVStack { /* ... */ }
                    .padding(.top, 60)
            }
            .coordinateSpace(name: "scroll")
            .onPreferenceChange(ScrollOffsetKey.self) { scrollOffset = $0 }

            // Glass header that intensifies on scroll
            Text("Title")
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding()
                .background(
                    scrollOffset < -10
                        ? AnyShapeStyle(.regularMaterial)
                        : AnyShapeStyle(.clear)
                )
                .animation(.easeInOut(duration: 0.2), value: scrollOffset < -10)
        }
    }
}
```

## Best Practices

1. **Never hardcode blur values** — Always use `.material` modifiers
2. **Test in both light and dark mode** — Glass renders differently
3. **Use `.continuous` corner style** — Apple's superellipse, not circular
4. **Keep glass layers to 3 max** — Too many layers reduce readability
5. **Use semantic foreground styles** — `.primary`, `.secondary`, not raw colors
6. **Add subtle borders** — `.white.opacity(0.15-0.25)` with 0.5pt stroke
7. **Shadow behind glass, not on it** — Shadow goes on the container
8. **Test on real devices** — Simulator doesn't perfectly render materials
