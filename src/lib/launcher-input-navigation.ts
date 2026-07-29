/**
 * Horizontal arrows belong to the launcher's tile grid only while the input
 * is truly empty. Once any text exists (including spaces around a query), the
 * browser must retain ArrowLeft/ArrowRight for native caret movement and text
 * selection.
 */
export function launcherInputUsesHorizontalGridNavigation(query: string) {
  return query.length === 0;
}
