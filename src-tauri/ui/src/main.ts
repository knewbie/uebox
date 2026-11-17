import { createApp } from 'vue'
import App from './App.vue'
import { createAppI18n } from './i18n'
import '@renderer/assets/styles/palette.generated.css'
import '@renderer/assets/styles/theme.css'

const app = createApp(App)
app.provide('size', '1.15em')
app.use(createAppI18n())
app.mount('#app')
