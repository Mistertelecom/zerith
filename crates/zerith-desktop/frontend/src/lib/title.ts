/** A conversation title from its first message: one line, cut at a word with
 * an ellipsis instead of mid-word. */
export function titleOf(prompt: string, max = 60): string {
  const line = prompt.replace(/!\[[^\]]*\]\(<?[^)]*>?\)/g, "").replace(/\s+/g, " ").trim();
  if (line.length <= max) return line || "Imagem";
  const cut = line.slice(0, max);
  const space = cut.lastIndexOf(" ");
  return (space > max / 2 ? cut.slice(0, space) : cut).replace(/[\s,.;:]+$/, "") + "…";
}
