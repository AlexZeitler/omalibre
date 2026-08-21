import QtQuick
import QtQuick.Layouts
import Quickshell.Io
import qs.Commons
import qs.Ui

// Bar widget: the books read last, a search over the library, and a way back
// into any of them.
//
// The reader itself is a terminal program. This widget never reads the journal
// on its own; it asks omalibre and draws what comes back, so the format of the
// journal stays the reader's business alone.
Panel {
  id: root
  moduleName: "alexzeitler.omalibre"
  ipcTarget: "alexzeitler.omalibre"

  // What the list currently shows: the books read last while the search box is
  // empty, matching books once something is typed.
  property var books: []
  // True when the binary is nowhere on disk. The panel then offers to fetch it.
  property bool notInstalled: false
  property string failure: ""
  property int cursor: 0

  // What the update check last answered. Empty until it has run, and empty
  // again on a machine that cannot reach GitHub.
  property string installedVersion: ""
  property string latestVersion: ""
  property bool updateAvailable: false

  // Three queries, because typing outruns the process that answers it: what was
  // typed last, what is being answered right now, and whether another run is owed.
  property string wantedQuery: ""
  property string runningQuery: ""
  property bool queryPending: false

  // As an escape rather than the glyph itself: a literal private-use
  // character does not survive every editor and pipe it passes through.
  readonly property string barIcon: "\uf02d"
  // The panel paints on its own opaque surface, so it takes the theme's
  // foreground. `barForeground` is a different colour: a see-through bar
  // shifts it to stay legible against the wallpaper, and that colour is
  // unreadable here.
  readonly property color fg: root.bar ? root.bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(fg, 1.45)
  readonly property string fontFamily: root.bar ? root.bar.fontFamily : Style.font.family

  // Both the widget and the menu entry go through this script rather than
  // calling the binary: it knows the three places omalibre can live.
  readonly property string runner: pathFromUrl(Qt.resolvedUrl("omalibre-run"))

  // The download goes through a script for the same reason the runner does:
  // it verifies the release signature before anything is unpacked, and that is
  // more shell than belongs in a QML string.
  readonly property string installer: pathFromUrl(Qt.resolvedUrl("omalibre-install"))

  // Comparing versions is shell work, not panel work: it reads the release tag
  // off a redirect and orders two version numbers. The panel only draws the
  // answer.
  readonly property string updateChecker: pathFromUrl(Qt.resolvedUrl("omalibre-update-check"))

  readonly property int recentCount: 5
  readonly property int matchCount: 8

  readonly property bool searching: root.runningQuery.trim() !== ""
  readonly property int limit: root.searching ? root.matchCount : root.recentCount
  readonly property var shownBooks: root.books.slice(0, root.limit)
  readonly property int hiddenCount: Math.max(0, root.books.length - root.limit)

  function pathFromUrl(url) {
    var value = String(url || "")
    if (value.indexOf("file://") === 0) return decodeURIComponent(value.substring(7))
    return value
  }

  function requestBooks(query) {
    root.wantedQuery = query
    if (listProcess.running) {
      root.queryPending = true
      return
    }
    root.startQuery()
  }

  function startQuery() {
    root.queryPending = false
    root.runningQuery = root.wantedQuery
    var needle = root.runningQuery.trim()
    listProcess.command = needle === ""
      ? [root.runner, "--recent", String(root.recentCount)]
      : [root.runner, "--list", "--json", "--filter", needle]
    listProcess.running = true
  }

  function refresh() {
    root.requestBooks(root.wantedQuery)
  }

  function takeBooks(raw) {
    var text = (raw || "").trim()
    if (text === "NOT_INSTALLED") {
      root.notInstalled = true
      root.failure = ""
      root.books = []
      return
    }
    root.notInstalled = false
    if (text === "") {
      root.failure = "omalibre said nothing"
      root.books = []
      return
    }
    try {
      var parsed = JSON.parse(text)
      root.books = Array.isArray(parsed) ? parsed : []
      root.failure = ""
    } catch (error) {
      root.books = []
      root.failure = "cannot read what omalibre printed"
    }
    root.cursor = Math.min(root.cursor, Math.max(0, root.shownBooks.length - 1))
  }

  // Always a fresh reader rather than a focus of the running one: a click names
  // a book, and focusing a window that shows a different book would answer a
  // different question than the one asked.
  function openBook(id) {
    if (!id) return
    openProcess.command = ["omarchy-launch-tui", "--app-id=org.omarchy.omalibre",
      root.runner, "--open", id]
    openProcess.running = true
    root.close()
  }

  // No book named, so the reader comes up on the library itself.
  function openLibrary() {
    openProcess.command = ["omarchy-launch-tui", "--app-id=org.omarchy.omalibre", root.runner]
    openProcess.running = true
    root.close()
  }

  function install() {
    installProcess.running = true
    root.close()
  }

  // Asks GitHub over the network, so never in the way of the list: the panel
  // draws its books first and the update line appears whenever the answer
  // arrives, or never.
  function checkForUpdate() {
    if (updateProcess.running) return
    updateProcess.running = true
  }

  function takeUpdate(raw) {
    try {
      var parsed = JSON.parse((raw || "").trim())
      root.installedVersion = parsed.installed || ""
      root.latestVersion = parsed.latest || ""
      root.updateAvailable = parsed.update === true
    } catch (error) {
      root.updateAvailable = false
    }
  }

  function moveCursor(delta) {
    if (root.shownBooks.length === 0) return
    root.cursor = Math.max(0, Math.min(root.shownBooks.length - 1, root.cursor + delta))
  }

  function activateCursor() {
    if (root.notInstalled) {
      root.install()
      return
    }
    var book = root.shownBooks[root.cursor]
    if (book) root.openBook(book.id)
  }

  function authorsOf(book) {
    if (!book || !book.authors || book.authors.length === 0) return ""
    return book.authors.join(", ")
  }

  function ago(iso) {
    if (!iso) return ""
    var then = new Date(iso)
    if (isNaN(then.getTime())) return ""
    var seconds = Math.max(0, (new Date().getTime() - then.getTime()) / 1000)
    if (seconds < 90) return "just now"
    var minutes = Math.round(seconds / 60)
    if (minutes < 60) return minutes + " min ago"
    var hours = Math.round(minutes / 60)
    if (hours < 24) return hours === 1 ? "an hour ago" : hours + " hours ago"
    var days = Math.round(hours / 24)
    if (days === 1) return "yesterday"
    if (days < 30) return days + " days ago"
    return Qt.formatDate(then, "d MMM yyyy")
  }

  onOpenedChanged: {
    if (opened) {
      root.cursor = 0
      searchField.text = ""
      root.requestBooks("")
      root.checkForUpdate()
    }
  }

  Component.onCompleted: root.requestBooks("")

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  // Typing outruns the process, so wait for a pause before asking again.
  Timer {
    id: typingTimer
    interval: 180
    onTriggered: root.requestBooks(searchField.text)
  }

  Process {
    id: listProcess
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.takeBooks(text)
    }
    onExited: if (root.queryPending) root.startQuery()
  }

  Process {
    id: openProcess
  }

  Process {
    id: updateProcess
    command: [root.updateChecker]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.takeUpdate(text)
    }
  }

  Process {
    id: installProcess
    command: ["omarchy-launch-tui", "--app-id=org.omarchy.omalibre-install",
      root.installer]
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.barIcon
    tooltipText: "Omalibre"
    onPressed: function(which) {
      if (root.opened) root.close()
      else root.open()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: searchField
    contentWidth: panel.fittedContentWidth(Style.space(440))
    contentHeight: panel.fittedContentHeight(contentColumn.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: searchField.activeFocus
      onCloseRequested: root.close()
      onMoveRequested: function(dx, dy) { root.moveCursor(dy) }
      onActivateRequested: root.activateCursor()

      ColumnLayout {
        id: contentColumn
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(10)

        RowLayout {
          Layout.fillWidth: true
          spacing: Style.space(8)

          Text {
            text: root.searching
              ? (root.books.length === 1 ? "1 book" : root.books.length + " books")
              : "Read last"
            color: root.fg
            font.family: root.fontFamily
            font.pixelSize: Style.font.title
            font.bold: true
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
          }

          Button {
            visible: !root.notInstalled
            text: "Library"
            foreground: root.fg
            tooltipText: "Open the whole library"
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.openLibrary()
          }

          Button {
            visible: !root.notInstalled
            text: "Refresh"
            foreground: root.fg
            tooltipText: "Ask Omalibre again"
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.refresh()
          }
        }

        // A newer release is out. The button goes through the same script the
        // first install goes through: it fetches releases/latest, checks the
        // signature and unpacks over what is there.
        RowLayout {
          visible: root.updateAvailable && !root.notInstalled
          Layout.fillWidth: true
          spacing: Style.space(8)

          Text {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            text: "Version " + root.latestVersion + " is out. You have "
              + root.installedVersion + "."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            wrapMode: Text.WordWrap
          }

          Button {
            text: "Update"
            foreground: root.fg
            active: true
            tooltipText: "Fetch and verify " + root.latestVersion
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.install()
          }
        }

        // The cursor keys belong to the list even while the box has focus, so
        // one can type a few letters and walk down to the book without reaching
        // for the mouse.
        TextField {
          id: searchField
          visible: !root.notInstalled
          Layout.fillWidth: true
          foreground: root.fg
          placeholderText: "Search title, author, series or tag"
          font.family: root.fontFamily
          onTextChanged: {
            root.cursor = 0
            typingTimer.restart()
          }
          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Down) { root.moveCursor(1); event.accepted = true }
            else if (event.key === Qt.Key_Up) { root.moveCursor(-1); event.accepted = true }
            else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
              root.activateCursor(); event.accepted = true
            } else if (event.key === Qt.Key_Escape) {
              root.close(); event.accepted = true
            }
          }
        }

        PanelSeparator {
          Layout.fillWidth: true
          foreground: root.fg
        }


        // omalibre is not on this machine. The bar can fetch it, which is the
        // whole point of shipping the widget with the reader.
        ColumnLayout {
          visible: root.notInstalled
          Layout.fillWidth: true
          spacing: Style.space(8)

          Text {
            Layout.fillWidth: true
            text: "Omalibre is not installed yet."
            color: root.fg
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }

          Text {
            Layout.fillWidth: true
            text: "One file, no dependencies. It lands in ~/.local/bin."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            wrapMode: Text.WordWrap
          }

          Button {
            text: "Install Omalibre"
            foreground: root.fg
            active: true
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.install()
          }
        }

        Text {
          visible: !root.notInstalled && root.failure !== ""
          Layout.fillWidth: true
          text: root.failure
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.WordWrap
        }

        Text {
          visible: !root.notInstalled && root.failure === "" && root.books.length === 0
          Layout.fillWidth: true
          text: root.searching
            ? "No book matches."
            : "No book read yet. Fill the library with: omalibre --scan ~/Books"
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.WordWrap
        }

        Repeater {
          model: root.notInstalled ? [] : root.shownBooks

          delegate: CursorSurface {
            id: row
            required property var modelData
            required property int index

            Layout.fillWidth: true
            implicitHeight: rowBody.implicitHeight + Style.space(16)
            foreground: root.fg
            hasCursor: root.cursor === row.index

            ColumnLayout {
              id: rowBody
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              anchors.leftMargin: Style.space(10)
              anchors.rightMargin: Style.space(10)
              spacing: Style.space(2)

              Text {
                Layout.fillWidth: true
                text: row.modelData.title + (row.modelData.missing ? "  (file missing)" : "")
                color: root.fg
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                elide: Text.ElideRight
              }

              Text {
                Layout.fillWidth: true
                visible: text !== ""
                text: {
                  var who = root.authorsOf(row.modelData)
                  var when = root.ago(row.modelData.lastRead)
                  if (who !== "" && when !== "") return who + "  ·  " + when
                  return who !== "" ? who : when
                }
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                elide: Text.ElideRight
              }
            }

            MouseArea {
              anchors.fill: parent
              hoverEnabled: true
              cursorShape: Qt.PointingHandCursor
              onContainsMouseChanged: if (containsMouse) root.cursor = row.index
              onClicked: root.openBook(row.modelData.id)
            }
          }
        }

        Text {
          visible: root.hiddenCount > 0
          Layout.fillWidth: true
          text: root.hiddenCount + " more not shown. Type to narrow it down."
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.WordWrap
        }
      }
    }
  }
}
