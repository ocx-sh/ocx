import type { Theme } from 'vitepress'
import DefaultTheme from 'vitepress/theme'
import 'virtual:group-icons.css'

import Tooltip from './components/Tooltip.vue'
import FileTree from './components/FileTree.vue'
import FileTreeNode from './components/FileTreeNode.vue'
import Stepper from './components/Stepper.vue'
import Tree from './components/Tree.vue'
import Node from './components/Node.vue'
import Steps from './components/Steps.vue'
import Step from './components/Step.vue'
import Description from './components/Description.vue'
import Terminal from './components/Terminal.vue'
import Frame from './components/Frame.vue'
import DependencyExplorer from './components/DependencyExplorer.vue'
import CopySnippet from './components/CopySnippet.vue'
import PackageCatalog from './components/PackageCatalog.vue'
import PackageDetail from './components/PackageDetail.vue'
import TagBadge from './components/TagBadge.vue'
import VersionTree from './components/VersionTree.vue'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component('Tooltip', Tooltip)
    app.component('FileTree', FileTree)
    app.component('FileTreeNode', FileTreeNode)
    app.component('Stepper', Stepper)
    // Declarative wrappers
    app.component('Tree', Tree)
    app.component('Node', Node)
    app.component('Steps', Steps)
    app.component('Step', Step)
    app.component('Description', Description)
    app.component('Terminal', Terminal)
    app.component('Frame', Frame)
    app.component('DependencyExplorer', DependencyExplorer)
    app.component('CopySnippet', CopySnippet)
    app.component('PackageCatalog', PackageCatalog)
    app.component('PackageDetail', PackageDetail)
    app.component('TagBadge', TagBadge)
    app.component('VersionTree', VersionTree)
  },
} satisfies Theme
