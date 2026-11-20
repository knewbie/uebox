<script lang="ts">
import { computed, defineComponent, h, type PropType, type VNodeChild } from 'vue'
import MarkdownIt from 'markdown-it'

type Format = 'plain' | 'markdown' | 'html'
const renderer = new MarkdownIt({ html: false, linkify: false })
const formatting = new Set([
  'p',
  'br',
  'hr',
  'pre',
  'code',
  'blockquote',
  'strong',
  'b',
  'em',
  'i',
  's',
  'u',
  'sub',
  'sup',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'ul',
  'ol',
  'li',
  'table',
  'thead',
  'tbody',
  'tr',
  'th',
  'td'
])
const discard = new Set(['script', 'style', 'iframe', 'object', 'embed', 'template', 'noscript'])

function visibleContent(markup: string): VNodeChild[] {
  // A template parses fragments outside the document. Only new text and formatting
  // nodes are mounted. Imported elements and attributes are never inserted into the UI.
  const template = document.createElement('template')
  template.innerHTML = markup
  let remaining = 20_000
  function children(parent: Node, depth: number): VNodeChild[] {
    const result: VNodeChild[] = []
    if (depth > 100) return result
    for (const node of parent.childNodes) {
      if (remaining-- <= 0) break
      if (node.nodeType === Node.TEXT_NODE) {
        result.push(node.textContent ?? '')
      } else if (node.nodeType === Node.ELEMENT_NODE) {
        const element = node as Element
        const name = element.localName
        if (element.namespaceURI !== 'http://www.w3.org/1999/xhtml' || discard.has(name)) continue
        const nested = children(element, depth + 1)
        if (formatting.has(name)) result.push(h(name, null, nested))
        else result.push(...nested)
      }
    }
    return result
  }
  return children(template.content, 0)
}

export default defineComponent({
  props: {
    content: { type: String, required: true },
    format: String as PropType<Format>
  },
  setup(props) {
    const body = computed(() => {
      if (!props.format || props.format === 'plain') return null
      return visibleContent(
        props.format === 'html' ? props.content : renderer.render(props.content)
      )
    })
    return () =>
      body.value === null
        ? h('pre', { class: 'note-plain', 'data-note-content': '' }, props.content)
        : h('div', { class: 'note-rich', 'data-note-content': '' }, body.value)
  }
})
</script>

<style scoped>
.note-plain {
  white-space: pre-wrap;
  font-family: inherit;
}
.note-plain,
.note-rich {
  overflow-wrap: anywhere;
}
.note-rich :deep(table) {
  border-collapse: collapse;
  max-inline-size: 100%;
}
.note-rich :deep(:is(td, th)) {
  border: 1px solid var(--color-border);
  padding: var(--space-1) var(--space-2);
}
.note-rich :deep(pre) {
  overflow-x: auto;
}
</style>
