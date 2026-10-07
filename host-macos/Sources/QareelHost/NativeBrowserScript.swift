import Foundation
import WebKit

@MainActor
enum NativeBrowserScript {
    static let world = WKContentWorld.world(name: "commission.browser")

    static var snapshot: String {
        """
        (() => {
          const label = node => (node.getAttribute('aria-label') || node.labels?.[0]?.innerText || node.getAttribute('placeholder') || node.innerText || node.getAttribute('title') || '').trim().slice(0, 240);
          const visible = node => {
            const rect = node.getBoundingClientRect();
            const style = getComputedStyle(node);
            return rect.width > 0 && rect.height > 0 && rect.bottom > 0 && rect.right > 0 && rect.top < innerHeight && rect.left < innerWidth && style.visibility === 'visible' && style.display !== 'none' && style.opacity !== '0';
          };
          const path = node => {
            const parts = [];
            for (let current = node; current && current.nodeType === 1 && current !== document.documentElement; current = current.parentElement) {
              if (current.id && document.querySelectorAll('#' + CSS.escape(current.id)).length === 1) { parts.unshift('#' + CSS.escape(current.id)); break; }
              let index = 1;
              for (let sibling = current.previousElementSibling; sibling; sibling = sibling.previousElementSibling) if (sibling.localName === current.localName) index++;
              parts.unshift(current.localName + ':nth-of-type(' + index + ')');
            }
            return parts.join('>').slice(0, 512);
          };
          const unique = selector => { try { return document.querySelectorAll(selector).length === 1; } catch { return false; } };
          const stable = value => value && value.length <= 80 && !/[0-9]{4,}/.test(value) && !value.includes(':');
          const hint = node => {
            for (const attribute of ['data-testid', 'data-test-id']) {
              const value = node.getAttribute(attribute);
              if (stable(value) && unique('[' + attribute + '=' + JSON.stringify(value) + ']')) return '[' + attribute + '=' + JSON.stringify(value) + ']';
            }
            if (stable(node.id) && unique('#' + CSS.escape(node.id))) return '#' + CSS.escape(node.id);
            const name = node.getAttribute('name');
            if (stable(name) && unique(node.localName + '[name=' + JSON.stringify(name) + ']')) return node.localName + '[name=' + JSON.stringify(name) + ']';
            return '';
          };
          const token = '\(UUID().uuidString)';
          const nodes = new Map();
          const elements = [];
          for (const node of document.querySelectorAll('a[href],button,input,textarea,select,[contenteditable="true"],[role="button"],[role="link"],[role="textbox"],[role="checkbox"],[role="combobox"]')) {
            if (elements.length >= 300) break;
            if (!visible(node) || node.disabled || node.getAttribute('aria-disabled') === 'true') continue;
            const ref = token + ':' + elements.length;
            const name = label(node);
            const value = node.type === 'password' ? '' : String(node.value ?? '').slice(0, 2000);
            nodes.set(ref, {node, name, value, password: node.type === 'password'});
            elements.push({ref, tag:node.localName, role:node.getAttribute('role') || '', name, value, sel:path(node), selector: hint(node)});
          }
          globalThis.__commissionSnapshot = {document, nodes, label, visible};
          return {url:location.href, title:document.title, text:(document.body?.innerText || '').slice(0, 16000), elements};
        })()
        """
    }

    static func target(reference: String, editing: Bool) -> String {
        let ref = String(decoding: JSONValue.string(reference).encoded(), as: UTF8.self)
        return """
        (() => {
          const snapshot = globalThis.__commissionSnapshot;
          const item = snapshot?.nodes.get(\(ref));
          if (!item || snapshot.document !== document || !item.node.isConnected) throw new Error('browser.stale_target: take a fresh snapshot');
          const node = item.node;
          if (!snapshot.visible(node) || node.disabled || node.getAttribute('aria-disabled') === 'true' || snapshot.label(node) !== item.name) throw new Error('browser.stale_target: target changed');
          if (item.password) throw new Error('browser.password_input: enter passwords manually');
          if (String(node.value ?? '').slice(0,2000) !== item.value) throw new Error('browser.stale_target: field changed');
          const rect = node.getBoundingClientRect();
          const x = Math.max(0, Math.min(innerWidth - 1, rect.left + rect.width / 2));
          const y = Math.max(0, Math.min(innerHeight - 1, rect.top + rect.height / 2));
          const hit = document.elementFromPoint(x,y);
          if (!hit || !(hit === node || node.contains(hit))) throw new Error('browser.target_occluded: target is covered');
          if (\(editing ? "true" : "false")) {
            if (!(node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node.isContentEditable) || node.readOnly) throw new Error('browser.target_not_editable: target cannot accept text');
            node.focus();
            if (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement) node.select();
            else { const range = document.createRange(); range.selectNodeContents(node); const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range); }
            if (document.activeElement !== node) throw new Error('browser.focus_changed: target did not retain focus');
          }
          const left = Math.max(0, rect.left), top = Math.max(0, rect.top);
          const right = Math.min(innerWidth, rect.right), bottom = Math.min(innerHeight, rect.bottom);
          return {x,y,bounds:{left,top,width:right-left,height:bottom-top}};
        })()
        """
    }

    static let focused = """
    (() => {
      const node = document.activeElement;
      if (!node || node.type === 'password') throw new Error('browser.password_input: enter passwords manually');
      if (!(node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node.isContentEditable) || node.disabled || node.readOnly) throw new Error('browser.target_not_editable: focus an editable control');
      return true;
    })()
    """
}
