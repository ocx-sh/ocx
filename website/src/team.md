---
layout: page
---

<script setup>
import {
  VPTeamPage,
  VPTeamPageTitle,
  VPTeamMembers
} from 'vitepress/theme'

const members = [
  {
    avatar: 'https://www.github.com/michael-herwig.png',
    name: 'Michael Herwig',
    title: 'Creator & Maintainer',
    // https://simpleicons.org/
    links: [
      { icon: 'linkedin', link: 'https://www.linkedin.com/in/herwigm' },
      { icon: 'github', link: 'https://www.github.com/michael-herwig' },
      { icon: 'gitlab', link: 'https://gitlab.com/michael-herwig' },
      { icon: 'spotify', link: 'https://open.spotify.com/user/1170878827?si=9c507e1cf5684d2a' },
    ]
  }
]
</script>

<VPTeamPage>
  <VPTeamPageTitle>
    <template #title>
      Team
    </template>
  </VPTeamPageTitle>
  <VPTeamMembers :members />
</VPTeamPage>
