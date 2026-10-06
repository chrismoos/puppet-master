import type { ReactNode } from "react";
import { useMemo } from "react";
import { Pressable, Text, type TextStyle, View, type ViewStyle } from "react-native";
import { openExternalUrl } from "../adapters/openLink";
import { Renderer, useMarkdown, type MarkedStyles } from "react-native-marked";

import { colors } from "../theme";

const mdStyles: MarkedStyles = {
  text: { color: colors.text, fontSize: 14 },
  h1: { color: colors.textBright, fontSize: 22, fontWeight: "700", marginTop: 16, marginBottom: 8 },
  h2: { color: colors.textBright, fontSize: 18, fontWeight: "700", marginTop: 14, marginBottom: 6 },
  h3: { color: colors.textBright, fontSize: 15, fontWeight: "600", marginTop: 12, marginBottom: 4 },
  h4: { color: colors.textBright, fontSize: 14, fontWeight: "600", marginTop: 10, marginBottom: 4 },
  h5: { color: colors.textBright, fontSize: 13, fontWeight: "600", marginTop: 8, marginBottom: 4 },
  h6: { color: colors.textMuted, fontSize: 13, fontWeight: "600", marginTop: 8, marginBottom: 4 },
  strong: { fontWeight: "700", color: colors.textBright },
  em: { fontStyle: "italic" },
  link: { color: colors.blue, textDecorationLine: "underline" },
  blockquote: { borderLeftWidth: 3, borderLeftColor: colors.line, paddingLeft: 12, marginVertical: 6 },
  codespan: { fontFamily: "Menlo", fontSize: 13, backgroundColor: colors.panelAlt, color: colors.textBright },
  code: { backgroundColor: colors.panel, padding: 12, borderRadius: 8, marginVertical: 8 },
  table: { borderWidth: 1, borderColor: colors.line, borderRadius: 6, marginVertical: 8 },
  tableRow: { borderBottomWidth: 0.5, borderColor: colors.lineSoft },
  tableCell: { padding: 8, borderColor: colors.lineSoft },
  list: { marginVertical: 4 },
  li: { color: colors.text, fontSize: 14 },
  hr: { borderBottomWidth: 1, borderColor: colors.line, marginVertical: 12 },
  paragraph: { marginVertical: 4 },
};

class SafeLinkRenderer extends Renderer {
  link(children: string | ReactNode[], href: string, linkStyle?: TextStyle): ReactNode {
    return (
      <Pressable key={this.getKey()} onPress={() => openExternalUrl(href)} accessibilityRole="link" accessibilityHint={href}>
        <Text style={[mdStyles.link, linkStyle]}>{children}</Text>
      </Pressable>
    );
  }

  getKey(): string {
    return super.getKey();
  }
}

export function PlanMarkdown({
  markdown,
  style,
}: {
  markdown: string;
  style?: ViewStyle;
}) {
  const renderer = useMemo(() => new SafeLinkRenderer(), []);
  const elements = useMarkdown(markdown || "", { renderer, styles: mdStyles });

  if (!markdown) return null;

  return <View style={style}>{elements}</View>;
}
