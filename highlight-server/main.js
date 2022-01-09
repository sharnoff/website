const net = require('net')
const prism = require('prismjs')
const loadLanguages = require('prismjs/components/')

// Constant for the supported languages. We can just restart the server any time we want to add more
// languages.
const allUsedLangs = ['rust']

loadLanguages(allUsedLangs)

const server = net.createServer((socket) => {
    // We need to set the encoding to utf8 in order to get string data
    socket.setEncoding('utf8')

    let buf = ""

    socket.on('data', (data) => {
        // If the data we're given doesn't end in a null byte, add it to the buffer and continue...
        if (data.length === 0 || data[data.length - 1] !== '\0') {
            buf += data
            return;
        }

        // Otherwise, the request is made up of everything we've seen so far, without the trailing
        // null byte.
        let request = buf + data.slice(0, -1)

        try {
            const { code, language } = JSON.parse(request)
            request = { code, language }
        } catch (e) {
            console.error('bad request data:', data, ',', e)
            socket.write(JSON.stringify({ "failure": "bad request data: " + e.toString() }) + '\n')
            socket.end()
            return
        }

        try {
            const grammar = prism.languages[request.language]
            if (!grammar) {
                throw 'no such language recognized'
            }

            const html = prism.highlight(request.code, grammar, request.language)
            socket.write(JSON.stringify({ "success": html }) + '\n')
        } catch (e) {
            console.error(`failed to highlight for lanuage '${request.language}':`, e)
            socket.write(JSON.stringify({ "failure": e.toString() }) + '\n')
        }

        // TODO: Ideally, we don't end the socket here, but flush with a null byte; that would allow
        // http-server to reuse sockets if it needed to.
        //
        // Currently, it's ok though.
        socket.end()
    })
})

server.listen(8001, 'localhost', () => { console.log('bound address; listening...') })
