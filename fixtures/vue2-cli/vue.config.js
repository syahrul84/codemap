const path = require('path')

module.exports = {
  configureWebpack: {
    resolve: {
      alias: {
        Components: path.resolve(__dirname, 'src/components/'),
        Store: path.resolve(__dirname, 'src/store/'),
      },
    },
  },
}
